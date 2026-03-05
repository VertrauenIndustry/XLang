use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, UserFuncName};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};

use crate::ast::{BinOp, Expr, IsPattern, Program, Stmt, Type, UnaryOp};
use crate::builtins;
use crate::comptime::lower_comptime;
use crate::memory;
use crate::runtime_args;
use crate::typecheck::{type_check, Signature};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImage {
    pub abi_hashes: HashMap<String, u64>,
    pub body_hashes: HashMap<String, u64>,
    pub code_ptrs: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePatchResult {
    pub patched_functions: Vec<String>,
    pub rejected_functions: Vec<String>,
    pub restart_required: bool,
}

#[derive(Default)]
struct NativeStringPool {
    next_handle: i64,
    by_handle: HashMap<i64, String>,
    by_text: HashMap<String, i64>,
}

impl NativeStringPool {
    fn reset(&mut self) {
        self.next_handle = -9_000_000_000_000_000_000;
        self.by_handle.clear();
        self.by_text.clear();
    }

    fn intern(&mut self, value: &str) -> i64 {
        if let Some(existing) = self.by_text.get(value).copied() {
            return existing;
        }
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1);
        let owned = value.to_string();
        self.by_handle.insert(handle, owned.clone());
        self.by_text.insert(owned, handle);
        handle
    }
}

thread_local! {
    static NATIVE_STRINGS: RefCell<NativeStringPool> = RefCell::new(NativeStringPool::default());
}

pub fn run_native(program: &Program) -> Result<i64, String> {
    let lowered = lower_comptime(program).map_err(|diags| {
        diags
            .into_iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let mut compiled = CompiledJit::build(&lowered)?;
    let main_id = compiled
        .func_ids
        .get("main")
        .copied()
        .ok_or_else(|| "entry function 'main' not found for native execution".to_string())?;
    compiled
        .module
        .finalize_definitions()
        .map_err(to_string_err)?;
    let code = compiled.module.get_finalized_function(main_id);
    let main_fn = unsafe {
        // SAFETY: `main` is declared and defined with the exact signature `fn() -> i64`.
        std::mem::transmute::<*const u8, fn() -> i64>(code)
    };
    Ok(main_fn())
}

pub fn build_native_image(program: &Program) -> Result<NativeImage, String> {
    let lowered = lower_comptime(program).map_err(|diags| {
        diags
            .into_iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let mut compiled = CompiledJit::build(&lowered)?;
    compiled
        .module
        .finalize_definitions()
        .map_err(to_string_err)?;
    let mut code_ptrs = HashMap::new();
    for (name, id) in &compiled.func_ids {
        let ptr = compiled.module.get_finalized_function(*id);
        code_ptrs.insert(name.clone(), ptr as usize);
    }
    Ok(NativeImage {
        abi_hashes: compiled.abi_hashes,
        body_hashes: compiled.body_hashes,
        code_ptrs,
    })
}

pub fn plan_native_patch(old_image: &NativeImage, new_image: &NativeImage) -> NativePatchResult {
    let mut patched_functions = Vec::new();
    let mut rejected_functions = Vec::new();
    let mut restart_required = false;

    for (name, new_body) in &new_image.body_hashes {
        match (
            old_image.abi_hashes.get(name),
            new_image.abi_hashes.get(name),
            old_image.body_hashes.get(name),
        ) {
            (Some(old_abi), Some(new_abi), Some(old_body)) => {
                if old_abi != new_abi {
                    rejected_functions.push(name.clone());
                    restart_required = true;
                    continue;
                }
                if old_body != new_body {
                    patched_functions.push(name.clone());
                }
            }
            _ => {
                rejected_functions.push(name.clone());
                restart_required = true;
            }
        }
    }

    for old_name in old_image.body_hashes.keys() {
        if !new_image.body_hashes.contains_key(old_name) {
            rejected_functions.push(old_name.clone());
            restart_required = true;
        }
    }

    patched_functions.sort();
    rejected_functions.sort();
    rejected_functions.dedup();

    NativePatchResult {
        patched_functions,
        rejected_functions,
        restart_required,
    }
}

struct CompiledJit {
    module: JITModule,
    func_ids: HashMap<String, FuncId>,
    abi_hashes: HashMap<String, u64>,
    body_hashes: HashMap<String, u64>,
}

impl CompiledJit {
    fn build(program: &Program) -> Result<Self, String> {
        reset_native_runtime();
        ensure_native_supported(program)?;
        let signatures = type_check(program).map_err(|diags| {
            diags
                .into_iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })?;

        let mut builder = JITBuilder::new(default_libcall_names()).map_err(to_string_err)?;
        builder.symbol("memcpy", memcpy_shim as *const u8);
        builder.symbol("memset", memset_shim as *const u8);
        builder.symbol("memmove", memmove_shim as *const u8);
        register_runtime_symbols(&mut builder);
        let mut module = JITModule::new(builder);

        let mut func_ids = HashMap::new();
        let mut runtime_ids = HashMap::new();
        let mut abi_hashes = HashMap::new();
        let mut body_hashes = HashMap::new();

        for f in &program.functions {
            let sig = signature_for(
                &module,
                &f.params,
                signatures.get(&f.name).map(|s| &s.ret).unwrap_or(&f.ret),
            )?;
            let id = module
                .declare_function(&f.name, Linkage::Export, &sig)
                .map_err(to_string_err)?;
            func_ids.insert(f.name.clone(), id);
            abi_hashes.insert(f.name.clone(), hash_signature(&f.params, &f.ret));
            body_hashes.insert(f.name.clone(), hash_body(&f.body));
        }

        for f in &program.functions {
            let id = *func_ids
                .get(&f.name)
                .ok_or_else(|| format!("missing declared function '{}'", f.name))?;
            let mut ctx = module.make_context();
            let effective_ret = signatures.get(&f.name).map(|s| &s.ret).unwrap_or(&f.ret);
            ctx.func.signature = signature_for(&module, &f.params, effective_ret)?;
            ctx.func.name = UserFuncName::user(0, id.as_u32());

            let mut fb_ctx = FunctionBuilderContext::new();
            let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let entry = fb.create_block();
            fb.append_block_params_for_function_params(entry);
            fb.switch_to_block(entry);
            fb.seal_block(entry);

            let mut vars: HashMap<String, Variable> = HashMap::new();
            let mut var_types: HashMap<String, Type> = HashMap::new();
            for (idx, param) in f.params.iter().enumerate() {
                let var = fb.declare_var(clif_ty(&param.ty)?);
                let val = fb.block_params(entry)[idx];
                fb.def_var(var, val);
                vars.insert(param.name.clone(), var);
                let inferred_ty = signatures
                    .get(&f.name)
                    .and_then(|sig| sig.params.get(idx))
                    .cloned()
                    .unwrap_or_else(|| normalize_type(&param.ty));
                var_types.insert(param.name.clone(), inferred_ty);
            }

            let mut terminated = false;
            let mut loop_stack: Vec<(ir::Block, ir::Block)> = Vec::new();
            for stmt in &f.body {
                compile_stmt(
                    stmt,
                    &mut fb,
                    &mut vars,
                    &mut var_types,
                    &func_ids,
                    &mut runtime_ids,
                    &mut module,
                    effective_ret,
                    &signatures,
                    &mut terminated,
                    &mut loop_stack,
                )?;
            }

            if !terminated {
                if f.ret == Type::Void {
                    fb.ins().return_(&[]);
                } else {
                    return Err(format!(
                        "function '{}' can fall through without returning a value",
                        f.name
                    ));
                }
            }
            fb.finalize();

            module
                .define_function(id, &mut ctx)
                .map_err(to_string_err)?;
            module.clear_context(&mut ctx);
        }

        Ok(Self {
            module,
            func_ids,
            abi_hashes,
            body_hashes,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_stmt(
    stmt: &Stmt,
    fb: &mut FunctionBuilder<'_>,
    vars: &mut HashMap<String, Variable>,
    var_types: &mut HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    ret_ty: &Type,
    signatures: &HashMap<String, Signature>,
    terminated: &mut bool,
    loop_stack: &mut Vec<(ir::Block, ir::Block)>,
) -> Result<(), String> {
    if *terminated {
        return Ok(());
    }
    match stmt {
        Stmt::Let { name, ty, expr, .. } => {
            let inferred_ty = infer_expr_type(expr, var_types, signatures)?;
            let val = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let declared_ty = if *ty == Type::Infer {
                inferred_ty
            } else {
                normalize_type(ty)
            };
            let var = fb.declare_var(clif_ty(&declared_ty)?);
            fb.def_var(var, val);
            vars.insert(name.clone(), var);
            var_types.insert(name.clone(), declared_ty);
        }
        Stmt::Assign { name, expr, .. } => {
            let inferred_ty = infer_expr_type(expr, var_types, signatures)?;
            let val = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let var = if let Some(existing) = vars.get(name).copied() {
                existing
            } else {
                let created = fb.declare_var(clif_ty(&inferred_ty)?);
                vars.insert(name.clone(), created);
                created
            };
            fb.def_var(var, val);
            var_types.entry(name.clone()).or_insert(inferred_ty);
        }
        Stmt::Return { expr, .. } => {
            let val = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            if *ret_ty == Type::Void {
                fb.ins().return_(&[]);
            } else {
                fb.ins().return_(&[val]);
            }
            *terminated = true;
        }
        Stmt::Expr { expr, .. } => {
            let _ = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
        }
        Stmt::IfIs {
            value,
            arms,
            else_body,
            ..
        } => {
            let switch_ty = infer_expr_type(value, var_types, signatures)?;
            let switch_val = compile_expr(
                value,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let merge_block = fb.create_block();
            let entry_check = fb.create_block();
            fb.ins().jump(entry_check, &[]);

            let mut path_reaches_merge = false;
            let mut current_check = entry_check;
            for arm in arms {
                fb.switch_to_block(current_check);
                fb.seal_block(current_check);

                let arm_block = fb.create_block();
                let next_check = fb.create_block();
                let arm_cond = compile_if_is_arm_condition(
                    switch_val,
                    &switch_ty,
                    &arm.patterns,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    signatures,
                )?;
                fb.ins().brif(arm_cond, arm_block, &[], next_check, &[]);

                fb.switch_to_block(arm_block);
                fb.seal_block(arm_block);
                let mut arm_vars = vars.clone();
                let mut arm_var_types = var_types.clone();
                let mut arm_terminated = false;
                for stmt in &arm.body {
                    compile_stmt(
                        stmt,
                        fb,
                        &mut arm_vars,
                        &mut arm_var_types,
                        func_ids,
                        runtime_ids,
                        module,
                        ret_ty,
                        signatures,
                        &mut arm_terminated,
                        loop_stack,
                    )?;
                }
                if !arm_terminated {
                    fb.ins().jump(merge_block, &[]);
                    path_reaches_merge = true;
                }
                current_check = next_check;
            }

            fb.switch_to_block(current_check);
            fb.seal_block(current_check);
            let mut else_terminated = false;
            if !else_body.is_empty() {
                let mut else_vars = vars.clone();
                let mut else_var_types = var_types.clone();
                for stmt in else_body {
                    compile_stmt(
                        stmt,
                        fb,
                        &mut else_vars,
                        &mut else_var_types,
                        func_ids,
                        runtime_ids,
                        module,
                        ret_ty,
                        signatures,
                        &mut else_terminated,
                        loop_stack,
                    )?;
                }
                if !else_terminated {
                    fb.ins().jump(merge_block, &[]);
                    path_reaches_merge = true;
                }
            } else {
                fb.ins().jump(merge_block, &[]);
                path_reaches_merge = true;
            }

            if path_reaches_merge {
                fb.switch_to_block(merge_block);
                fb.seal_block(merge_block);
            } else {
                *terminated = true;
            }
        }
        Stmt::If {
            condition,
            then_body,
            elif_arms,
            else_body,
            ..
        } => {
            let merge_block = fb.create_block();
            let then_block = fb.create_block();
            let final_else_block = fb.create_block();
            let cond = compile_truthy_condition(
                condition,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            fb.ins().brif(cond, then_block, &[], final_else_block, &[]);

            let mut path_reaches_merge = false;

            fb.switch_to_block(then_block);
            fb.seal_block(then_block);
            let mut then_vars = vars.clone();
            let mut then_var_types = var_types.clone();
            let mut then_terminated = false;
            for stmt in then_body {
                compile_stmt(
                    stmt,
                    fb,
                    &mut then_vars,
                    &mut then_var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    ret_ty,
                    signatures,
                    &mut then_terminated,
                    loop_stack,
                )?;
            }
            if !then_terminated {
                fb.ins().jump(merge_block, &[]);
                path_reaches_merge = true;
            }

            let mut current_else_check = final_else_block;
            for arm in elif_arms {
                fb.switch_to_block(current_else_check);
                fb.seal_block(current_else_check);
                let arm_block = fb.create_block();
                let next_else = fb.create_block();
                let arm_cond = compile_truthy_condition(
                    &arm.condition,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    signatures,
                )?;
                fb.ins().brif(arm_cond, arm_block, &[], next_else, &[]);

                fb.switch_to_block(arm_block);
                fb.seal_block(arm_block);
                let mut arm_vars = vars.clone();
                let mut arm_var_types = var_types.clone();
                let mut arm_terminated = false;
                for stmt in &arm.body {
                    compile_stmt(
                        stmt,
                        fb,
                        &mut arm_vars,
                        &mut arm_var_types,
                        func_ids,
                        runtime_ids,
                        module,
                        ret_ty,
                        signatures,
                        &mut arm_terminated,
                        loop_stack,
                    )?;
                }
                if !arm_terminated {
                    fb.ins().jump(merge_block, &[]);
                    path_reaches_merge = true;
                }
                current_else_check = next_else;
            }

            fb.switch_to_block(current_else_check);
            fb.seal_block(current_else_check);
            let mut else_terminated = false;
            if !else_body.is_empty() {
                let mut else_vars = vars.clone();
                let mut else_var_types = var_types.clone();
                for stmt in else_body {
                    compile_stmt(
                        stmt,
                        fb,
                        &mut else_vars,
                        &mut else_var_types,
                        func_ids,
                        runtime_ids,
                        module,
                        ret_ty,
                        signatures,
                        &mut else_terminated,
                        loop_stack,
                    )?;
                }
                if !else_terminated {
                    fb.ins().jump(merge_block, &[]);
                    path_reaches_merge = true;
                }
            } else {
                fb.ins().jump(merge_block, &[]);
                path_reaches_merge = true;
            }

            if path_reaches_merge {
                fb.switch_to_block(merge_block);
                fb.seal_block(merge_block);
            } else {
                *terminated = true;
            }
        }
        Stmt::While {
            condition, body, ..
        } => {
            let loop_head = fb.create_block();
            let loop_body = fb.create_block();
            let loop_exit = fb.create_block();

            fb.ins().jump(loop_head, &[]);
            fb.switch_to_block(loop_head);
            let cond = compile_truthy_condition(
                condition,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            fb.ins().brif(cond, loop_body, &[], loop_exit, &[]);

            fb.switch_to_block(loop_body);
            fb.seal_block(loop_body);
            loop_stack.push((loop_exit, loop_head));
            let mut body_terminated = false;
            for stmt in body {
                compile_stmt(
                    stmt,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    ret_ty,
                    signatures,
                    &mut body_terminated,
                    loop_stack,
                )?;
            }
            loop_stack.pop();
            if !body_terminated {
                fb.ins().jump(loop_head, &[]);
            }

            fb.switch_to_block(loop_exit);
            fb.seal_block(loop_head);
            fb.seal_block(loop_exit);
        }
        Stmt::ForRange {
            var,
            start,
            end,
            step,
            body,
            ..
        } => {
            let start_val = compile_expr(
                start,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let end_val = compile_expr(
                end,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let step_val = if let Some(step_expr) = step {
                compile_expr(
                    step_expr,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    signatures,
                )?
            } else {
                fb.ins().iconst(types::I64, 1)
            };
            let idx_var = fb.declare_var(types::I64);
            let end_var = fb.declare_var(types::I64);
            let step_var = fb.declare_var(types::I64);
            fb.def_var(idx_var, start_val);
            fb.def_var(end_var, end_val);
            fb.def_var(step_var, step_val);

            let previous_var = vars.insert(var.clone(), idx_var);
            let previous_ty = var_types.insert(var.clone(), Type::I64);

            let loop_head = fb.create_block();
            let loop_body = fb.create_block();
            let loop_step = fb.create_block();
            let loop_exit = fb.create_block();

            fb.ins().jump(loop_head, &[]);
            fb.switch_to_block(loop_head);
            let cur = fb.use_var(idx_var);
            let lim = fb.use_var(end_var);
            let step_now = fb.use_var(step_var);
            let step_pos = fb.ins().icmp_imm(IntCC::SignedGreaterThan, step_now, 0);
            let step_neg = fb.ins().icmp_imm(IntCC::SignedLessThan, step_now, 0);
            let lt = fb.ins().icmp(IntCC::SignedLessThan, cur, lim);
            let gt = fb.ins().icmp(IntCC::SignedGreaterThan, cur, lim);
            let step_pos_i = bool_to_i64(fb, step_pos);
            let step_neg_i = bool_to_i64(fb, step_neg);
            let lt_i = bool_to_i64(fb, lt);
            let gt_i = bool_to_i64(fb, gt);
            let pos_cond = fb.ins().band(step_pos_i, lt_i);
            let neg_cond = fb.ins().band(step_neg_i, gt_i);
            let cond_i = fb.ins().bor(pos_cond, neg_cond);
            let cond = fb.ins().icmp_imm(IntCC::NotEqual, cond_i, 0);
            fb.ins().brif(cond, loop_body, &[], loop_exit, &[]);

            fb.switch_to_block(loop_body);
            fb.seal_block(loop_body);
            loop_stack.push((loop_exit, loop_step));
            let mut body_terminated = false;
            for stmt in body {
                compile_stmt(
                    stmt,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    ret_ty,
                    signatures,
                    &mut body_terminated,
                    loop_stack,
                )?;
            }
            loop_stack.pop();
            if !body_terminated {
                fb.ins().jump(loop_step, &[]);
            }

            fb.switch_to_block(loop_step);
            fb.seal_block(loop_step);
            let cur = fb.use_var(idx_var);
            let step_now = fb.use_var(step_var);
            let next = fb.ins().iadd(cur, step_now);
            fb.def_var(idx_var, next);
            fb.ins().jump(loop_head, &[]);

            fb.switch_to_block(loop_exit);
            fb.seal_block(loop_head);
            fb.seal_block(loop_exit);

            if let Some(prev) = previous_var {
                vars.insert(var.clone(), prev);
            } else {
                vars.remove(var);
            }
            if let Some(prev) = previous_ty {
                var_types.insert(var.clone(), prev);
            } else {
                var_types.remove(var);
            }
        }
        Stmt::Comptime { body, .. } => {
            for stmt in body {
                compile_stmt(
                    stmt,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    ret_ty,
                    signatures,
                    terminated,
                    loop_stack,
                )?;
                if *terminated {
                    break;
                }
            }
        }
        Stmt::ThreadCall { .. } | Stmt::ThreadWhile { .. } => {
            return Err(
                "native backend does not support thread() syntax yet; interpreter fallback required"
                    .to_string(),
            );
        }
        Stmt::Pass { .. } => {}
        Stmt::Break { .. } => {
            let Some((break_block, _)) = loop_stack.last().copied() else {
                return Err("`break` can only appear inside a loop".to_string());
            };
            fb.ins().jump(break_block, &[]);
            *terminated = true;
        }
        Stmt::Continue { .. } => {
            let Some((_, continue_block)) = loop_stack.last().copied() else {
                return Err("`continue` can only appear inside a loop".to_string());
            };
            fb.ins().jump(continue_block, &[]);
            *terminated = true;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compile_truthy_condition(
    condition: &Expr,
    fb: &mut FunctionBuilder<'_>,
    vars: &HashMap<String, Variable>,
    var_types: &HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    signatures: &HashMap<String, Signature>,
) -> Result<ir::Value, String> {
    let cond_val = compile_expr(
        condition,
        fb,
        vars,
        var_types,
        func_ids,
        runtime_ids,
        module,
        signatures,
    )?;
    if infer_expr_type(condition, var_types, signatures)? == Type::Str {
        let call = call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_truthy",
            &[types::I64],
            Some(types::I64),
            &[cond_val],
        )?;
        return Ok(fb.ins().icmp_imm(IntCC::NotEqual, call, 0));
    }
    Ok(fb.ins().icmp_imm(IntCC::NotEqual, cond_val, 0))
}

#[allow(clippy::too_many_arguments)]
fn compile_if_is_arm_condition(
    switch_val: ir::Value,
    switch_ty: &Type,
    patterns: &[IsPattern],
    fb: &mut FunctionBuilder<'_>,
    vars: &HashMap<String, Variable>,
    var_types: &HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    signatures: &HashMap<String, Signature>,
) -> Result<ir::Value, String> {
    let mut cond_i = fb.ins().iconst(types::I64, 0);
    for pattern in patterns {
        let matches = compile_if_is_pattern_condition(
            switch_val,
            switch_ty,
            pattern,
            fb,
            vars,
            var_types,
            func_ids,
            runtime_ids,
            module,
            signatures,
        )?;
        let match_i = bool_to_i64(fb, matches);
        cond_i = fb.ins().bor(cond_i, match_i);
    }
    Ok(fb.ins().icmp_imm(IntCC::NotEqual, cond_i, 0))
}

#[allow(clippy::too_many_arguments)]
fn compile_if_is_pattern_condition(
    switch_val: ir::Value,
    switch_ty: &Type,
    pattern: &IsPattern,
    fb: &mut FunctionBuilder<'_>,
    vars: &HashMap<String, Variable>,
    var_types: &HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    signatures: &HashMap<String, Signature>,
) -> Result<ir::Value, String> {
    match pattern {
        IsPattern::Value(expr) => {
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            if *switch_ty == Type::Str {
                if infer_expr_type(expr, var_types, signatures)? != Type::Str {
                    return Err("string `if ... is` arm requires string patterns".to_string());
                }
                let eq = call_runtime(
                    fb,
                    module,
                    runtime_ids,
                    "__xlang_str_eq",
                    &[types::I64, types::I64],
                    Some(types::I64),
                    &[switch_val, rhs],
                )?;
                Ok(fb.ins().icmp_imm(IntCC::NotEqual, eq, 0))
            } else {
                Ok(fb.ins().icmp(IntCC::Equal, switch_val, rhs))
            }
        }
        IsPattern::Ne(expr) => {
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            if *switch_ty == Type::Str {
                let eq = call_runtime(
                    fb,
                    module,
                    runtime_ids,
                    "__xlang_str_eq",
                    &[types::I64, types::I64],
                    Some(types::I64),
                    &[switch_val, rhs],
                )?;
                Ok(fb.ins().icmp_imm(IntCC::Equal, eq, 0))
            } else {
                Ok(fb.ins().icmp(IntCC::NotEqual, switch_val, rhs))
            }
        }
        IsPattern::Lt(expr) => {
            if *switch_ty == Type::Str {
                return Err(
                    "string `if ... is` supports only value/!= and string predicates".to_string(),
                );
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            Ok(fb.ins().icmp(IntCC::SignedLessThan, switch_val, rhs))
        }
        IsPattern::Le(expr) => {
            if *switch_ty == Type::Str {
                return Err(
                    "string `if ... is` supports only value/!= and string predicates".to_string(),
                );
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            Ok(fb.ins().icmp(IntCC::SignedLessThanOrEqual, switch_val, rhs))
        }
        IsPattern::Gt(expr) => {
            if *switch_ty == Type::Str {
                return Err(
                    "string `if ... is` supports only value/!= and string predicates".to_string(),
                );
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            Ok(fb.ins().icmp(IntCC::SignedGreaterThan, switch_val, rhs))
        }
        IsPattern::Ge(expr) => {
            if *switch_ty == Type::Str {
                return Err(
                    "string `if ... is` supports only value/!= and string predicates".to_string(),
                );
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            Ok(fb
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, switch_val, rhs))
        }
        IsPattern::Range { start, end } => {
            if *switch_ty == Type::Str {
                return Err("string `if ... is` does not support range patterns".to_string());
            }
            let start_v = compile_expr(
                start,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let end_v = compile_expr(
                end,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let ge_start = fb
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, switch_val, start_v);
            let lt_end = fb.ins().icmp(IntCC::SignedLessThan, switch_val, end_v);
            let ge_i = bool_to_i64(fb, ge_start);
            let lt_i = bool_to_i64(fb, lt_end);
            let both = fb.ins().band(ge_i, lt_i);
            Ok(fb.ins().icmp_imm(IntCC::NotEqual, both, 0))
        }
        IsPattern::StartsWith(expr) => {
            if *switch_ty != Type::Str {
                return Err("`starts_with` pattern requires string switch value".to_string());
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let out = call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_str_starts_with",
                &[types::I64, types::I64],
                Some(types::I64),
                &[switch_val, rhs],
            )?;
            Ok(fb.ins().icmp_imm(IntCC::NotEqual, out, 0))
        }
        IsPattern::EndsWith(expr) => {
            if *switch_ty != Type::Str {
                return Err("`ends_with` pattern requires string switch value".to_string());
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let out = call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_str_ends_with",
                &[types::I64, types::I64],
                Some(types::I64),
                &[switch_val, rhs],
            )?;
            Ok(fb.ins().icmp_imm(IntCC::NotEqual, out, 0))
        }
        IsPattern::Contains(expr) => {
            if *switch_ty != Type::Str {
                return Err("`contains` pattern requires string switch value".to_string());
            }
            let rhs = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let out = call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_str_contains",
                &[types::I64, types::I64],
                Some(types::I64),
                &[switch_val, rhs],
            )?;
            Ok(fb.ins().icmp_imm(IntCC::NotEqual, out, 0))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_expr(
    expr: &Expr,
    fb: &mut FunctionBuilder<'_>,
    vars: &HashMap<String, Variable>,
    var_types: &HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    signatures: &HashMap<String, Signature>,
) -> Result<ir::Value, String> {
    match expr {
        Expr::Int(v) => Ok(fb.ins().iconst(types::I64, *v)),
        Expr::Bool(v) => Ok(fb.ins().iconst(types::I64, if *v { 1 } else { 0 })),
        Expr::Str(v) => Ok(fb.ins().iconst(types::I64, intern_native_string(v))),
        Expr::Var(name) | Expr::Move(name) => {
            let Some(var) = vars.get(name).copied() else {
                return Err(format!("unknown variable '{name}'"));
            };
            Ok(fb.use_var(var))
        }
        Expr::Unary { op, expr } => {
            let v = compile_expr(
                expr,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            match op {
                UnaryOp::Neg => Ok(fb.ins().ineg(v)),
                UnaryOp::Not => {
                    let b = fb.ins().icmp_imm(IntCC::Equal, v, 0);
                    Ok(bool_to_i64(fb, b))
                }
            }
        }
        Expr::Binary { op, left, right } => {
            let left_ty = infer_expr_type(left, var_types, signatures)?;
            let right_ty = infer_expr_type(right, var_types, signatures)?;
            let l = compile_expr(
                left,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let r = compile_expr(
                right,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )?;
            let val = match op {
                BinOp::Add => {
                    if left_ty == Type::Str && right_ty == Type::Str {
                        call_runtime(
                            fb,
                            module,
                            runtime_ids,
                            "__xlang_str_concat",
                            &[types::I64, types::I64],
                            Some(types::I64),
                            &[l, r],
                        )?
                    } else {
                        fb.ins().iadd(l, r)
                    }
                }
                BinOp::Sub => fb.ins().isub(l, r),
                BinOp::Mul => fb.ins().imul(l, r),
                BinOp::Div => fb.ins().sdiv(l, r),
                BinOp::Mod => fb.ins().srem(l, r),
                BinOp::Eq => {
                    if left_ty == Type::Str && right_ty == Type::Str {
                        call_runtime(
                            fb,
                            module,
                            runtime_ids,
                            "__xlang_str_eq",
                            &[types::I64, types::I64],
                            Some(types::I64),
                            &[l, r],
                        )?
                    } else {
                        let b = fb.ins().icmp(IntCC::Equal, l, r);
                        bool_to_i64(fb, b)
                    }
                }
                BinOp::Ne => {
                    if left_ty == Type::Str && right_ty == Type::Str {
                        let eq = call_runtime(
                            fb,
                            module,
                            runtime_ids,
                            "__xlang_str_eq",
                            &[types::I64, types::I64],
                            Some(types::I64),
                            &[l, r],
                        )?;
                        let is_zero = fb.ins().icmp_imm(IntCC::Equal, eq, 0);
                        bool_to_i64(fb, is_zero)
                    } else {
                        let b = fb.ins().icmp(IntCC::NotEqual, l, r);
                        bool_to_i64(fb, b)
                    }
                }
                BinOp::Lt => {
                    let b = fb.ins().icmp(IntCC::SignedLessThan, l, r);
                    bool_to_i64(fb, b)
                }
                BinOp::Le => {
                    let b = fb.ins().icmp(IntCC::SignedLessThanOrEqual, l, r);
                    bool_to_i64(fb, b)
                }
                BinOp::Gt => {
                    let b = fb.ins().icmp(IntCC::SignedGreaterThan, l, r);
                    bool_to_i64(fb, b)
                }
                BinOp::Ge => {
                    let b = fb.ins().icmp(IntCC::SignedGreaterThanOrEqual, l, r);
                    bool_to_i64(fb, b)
                }
                BinOp::And => {
                    let lb = fb.ins().icmp_imm(IntCC::NotEqual, l, 0);
                    let rb = fb.ins().icmp_imm(IntCC::NotEqual, r, 0);
                    let l64 = bool_to_i64(fb, lb);
                    let r64 = bool_to_i64(fb, rb);
                    fb.ins().band(l64, r64)
                }
                BinOp::Or => {
                    let lb = fb.ins().icmp_imm(IntCC::NotEqual, l, 0);
                    let rb = fb.ins().icmp_imm(IntCC::NotEqual, r, 0);
                    let l64 = bool_to_i64(fb, lb);
                    let r64 = bool_to_i64(fb, rb);
                    fb.ins().bor(l64, r64)
                }
            };
            Ok(val)
        }
        Expr::Call { name, args } => {
            if let Some(v) = compile_builtin_call(
                name,
                args,
                fb,
                vars,
                var_types,
                func_ids,
                runtime_ids,
                module,
                signatures,
            )? {
                return Ok(v);
            }
            let Some(func_id) = func_ids.get(name).copied() else {
                return Err(format!("unknown function '{name}'"));
            };
            let mut arg_values = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(compile_expr(
                    arg,
                    fb,
                    vars,
                    var_types,
                    func_ids,
                    runtime_ids,
                    module,
                    signatures,
                )?);
            }
            let callee = module.declare_func_in_func(func_id, fb.func);
            let call = fb.ins().call(callee, &arg_values);
            let results = fb.inst_results(call);
            Ok(results
                .first()
                .copied()
                .unwrap_or_else(|| fb.ins().iconst(types::I64, 0)))
        }
    }
}

fn bool_to_i64(fb: &mut FunctionBuilder<'_>, b: ir::Value) -> ir::Value {
    let one = fb.ins().iconst(types::I64, 1);
    let zero = fb.ins().iconst(types::I64, 0);
    fb.ins().select(b, one, zero)
}

#[allow(clippy::too_many_arguments)]
fn compile_builtin_call(
    name: &str,
    args: &[Expr],
    fb: &mut FunctionBuilder<'_>,
    vars: &HashMap<String, Variable>,
    var_types: &HashMap<String, Type>,
    func_ids: &HashMap<String, FuncId>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
    signatures: &HashMap<String, Signature>,
) -> Result<Option<ir::Value>, String> {
    if !builtins::is_builtin(name) {
        return Ok(None);
    }
    let mut values = Vec::with_capacity(args.len());
    let mut arg_tys = Vec::with_capacity(args.len());
    for arg in args {
        arg_tys.push(infer_expr_type(arg, var_types, signatures)?);
        values.push(compile_expr(
            arg,
            fb,
            vars,
            var_types,
            func_ids,
            runtime_ids,
            module,
            signatures,
        )?);
    }
    if let Some(ret) = builtins::return_type(name, &arg_tys) {
        ret?;
    }

    let result = match name {
        "write" => {
            if values.is_empty() {
                return Err("builtin 'write' expects at least 1 argument".to_string());
            }
            for (idx, (value, ty)) in values.iter().zip(&arg_tys).enumerate() {
                if idx > 0 {
                    let _ = call_runtime(
                        fb,
                        module,
                        runtime_ids,
                        "__xlang_print_space",
                        &[],
                        Some(types::I64),
                        &[],
                    )?;
                }
                emit_print_value(*value, ty, fb, runtime_ids, module)?;
            }
            let _ = call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_stdout_flush",
                &[],
                Some(types::I64),
                &[],
            )?;
            fb.ins().iconst(types::I64, 0)
        }
        "print" | "println" => {
            for (idx, (value, ty)) in values.iter().zip(&arg_tys).enumerate() {
                if idx > 0 {
                    let _ = call_runtime(
                        fb,
                        module,
                        runtime_ids,
                        "__xlang_print_space",
                        &[],
                        Some(types::I64),
                        &[],
                    )?;
                }
                emit_print_value(*value, ty, fb, runtime_ids, module)?;
            }
            let _ = call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_print_newline",
                &[],
                Some(types::I64),
                &[],
            )?;
            fb.ins().iconst(types::I64, 0)
        }
        "input" => {
            if values.is_empty() {
                call_runtime(
                    fb,
                    module,
                    runtime_ids,
                    "__xlang_input0",
                    &[],
                    Some(types::I64),
                    &[],
                )?
            } else {
                call_runtime(
                    fb,
                    module,
                    runtime_ids,
                    "__xlang_input1",
                    &[types::I64],
                    Some(types::I64),
                    &[values[0]],
                )?
            }
        }
        "argc" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_argc",
            &[],
            Some(types::I64),
            &[],
        )?,
        "argv" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_argv",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "sleep_ms" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_sleep_ms",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "len" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_len",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "clock_ms" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_clock_ms",
            &[],
            Some(types::I64),
            &[],
        )?,
        "assert" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_assert",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "panic" => match arg_tys[0] {
            Type::Str => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_panic_str",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
            Type::Bool => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_panic_bool",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
            _ => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_panic_i64",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
        },
        "exit" => values[0],
        "max" => {
            let gt = fb
                .ins()
                .icmp(IntCC::SignedGreaterThan, values[0], values[1]);
            fb.ins().select(gt, values[0], values[1])
        }
        "min" => {
            let lt = fb.ins().icmp(IntCC::SignedLessThan, values[0], values[1]);
            fb.ins().select(lt, values[0], values[1])
        }
        "abs" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_abs_i64",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "pow" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_pow_i64",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "clamp" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_clamp_i64",
            &[types::I64, types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1], values[2]],
        )?,
        "str" => match arg_tys[0] {
            Type::Str => values[0],
            Type::Bool => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_to_str_bool",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
            _ => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_to_str_i64",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
        },
        "int" => match arg_tys[0] {
            Type::Str => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_to_int_str",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
            Type::Bool => values[0],
            _ => values[0],
        },
        "bool" => match arg_tys[0] {
            Type::Str => call_runtime(
                fb,
                module,
                runtime_ids,
                "__xlang_str_truthy",
                &[types::I64],
                Some(types::I64),
                &[values[0]],
            )?,
            Type::Bool => values[0],
            _ => {
                let non_zero = fb.ins().icmp_imm(IntCC::NotEqual, values[0], 0);
                bool_to_i64(fb, non_zero)
            }
        },
        "ord" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ord_str",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "chr" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_chr_i64",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "contains" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_contains",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "find" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_find",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "starts_with" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_starts_with",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "ends_with" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_ends_with",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "replace" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_replace",
            &[types::I64, types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1], values[2]],
        )?,
        "split" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_split_n",
            &[types::I64, types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1], values[2]],
        )?,
        "join" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_join",
            &[types::I64, types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1], values[2]],
        )?,
        "trim" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_trim",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "upper" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_upper",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "lower" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_str_lower",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "ptr_can_read" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_can_read",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "ptr_can_write" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_can_write",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "ptr_read8" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_read8",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "ptr_write8" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_write8",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "ptr_read64" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_read64",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "ptr_write64" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ptr_write64",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "ct_hash" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ct_hash_str",
            &[types::I64],
            Some(types::I64),
            &[values[0]],
        )?,
        "ct_xor" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_ct_xor_str",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        "xor_decode" => call_runtime(
            fb,
            module,
            runtime_ids,
            "__xlang_xor_decode",
            &[types::I64, types::I64],
            Some(types::I64),
            &[values[0], values[1]],
        )?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

fn emit_print_value(
    value: ir::Value,
    ty: &Type,
    fb: &mut FunctionBuilder<'_>,
    runtime_ids: &mut HashMap<String, FuncId>,
    module: &mut JITModule,
) -> Result<(), String> {
    let name = match ty {
        Type::Str => "__xlang_print_str",
        Type::Bool => "__xlang_print_bool",
        _ => "__xlang_print_i64",
    };
    let _ = call_runtime(
        fb,
        module,
        runtime_ids,
        name,
        &[types::I64],
        Some(types::I64),
        &[value],
    )?;
    Ok(())
}

fn call_runtime(
    fb: &mut FunctionBuilder<'_>,
    module: &mut JITModule,
    runtime_ids: &mut HashMap<String, FuncId>,
    name: &str,
    params: &[ir::Type],
    ret: Option<ir::Type>,
    args: &[ir::Value],
) -> Result<ir::Value, String> {
    let id = ensure_runtime_fn(module, runtime_ids, name, params, ret)?;
    let callee = module.declare_func_in_func(id, fb.func);
    let call = fb.ins().call(callee, args);
    let results = fb.inst_results(call);
    Ok(results
        .first()
        .copied()
        .unwrap_or_else(|| fb.ins().iconst(types::I64, 0)))
}

fn ensure_runtime_fn(
    module: &mut JITModule,
    runtime_ids: &mut HashMap<String, FuncId>,
    name: &str,
    params: &[ir::Type],
    ret: Option<ir::Type>,
) -> Result<FuncId, String> {
    if let Some(id) = runtime_ids.get(name).copied() {
        return Ok(id);
    }
    let mut sig = module.make_signature();
    for ty in params {
        sig.params.push(AbiParam::new(*ty));
    }
    if let Some(ret_ty) = ret {
        sig.returns.push(AbiParam::new(ret_ty));
    }
    let id = module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(to_string_err)?;
    runtime_ids.insert(name.to_string(), id);
    Ok(id)
}

fn infer_expr_type(
    expr: &Expr,
    var_types: &HashMap<String, Type>,
    signatures: &HashMap<String, Signature>,
) -> Result<Type, String> {
    match expr {
        Expr::Int(_) => Ok(Type::I64),
        Expr::Bool(_) => Ok(Type::Bool),
        Expr::Str(_) => Ok(Type::Str),
        Expr::Var(name) | Expr::Move(name) => var_types
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown variable '{name}'")),
        Expr::Unary { op, .. } => match op {
            UnaryOp::Neg => Ok(Type::I64),
            UnaryOp::Not => Ok(Type::Bool),
        },
        Expr::Binary { op, left, right } => {
            let l = infer_expr_type(left, var_types, signatures)?;
            let r = infer_expr_type(right, var_types, signatures)?;
            match op {
                BinOp::Add if l == Type::Str && r == Type::Str => Ok(Type::Str),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => Ok(Type::I64),
                BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or => Ok(Type::Bool),
            }
        }
        Expr::Call { name, args } => {
            let arg_types = args
                .iter()
                .map(|a| infer_expr_type(a, var_types, signatures))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(ret) = builtins::return_type(name, &arg_types) {
                let ty = ret?;
                return Ok(normalize_type(&ty));
            }
            signatures
                .get(name)
                .map(|sig| normalize_type(&sig.ret))
                .ok_or_else(|| format!("unknown function '{name}'"))
        }
    }
}

fn normalize_type(ty: &Type) -> Type {
    match ty {
        Type::Infer => Type::I64,
        other => other.clone(),
    }
}

fn signature_for(
    module: &JITModule,
    params: &[crate::ast::Param],
    ret: &Type,
) -> Result<ir::Signature, String> {
    let mut sig = module.make_signature();
    for p in params {
        sig.params.push(AbiParam::new(clif_ty(&p.ty)?));
    }
    if *ret != Type::Void {
        sig.returns.push(AbiParam::new(clif_ty(ret)?));
    }
    Ok(sig)
}

fn clif_ty(ty: &Type) -> Result<ir::Type, String> {
    match ty {
        Type::Infer | Type::I64 | Type::Bool | Type::Str => Ok(types::I64),
        Type::Void => Err("void is not a value type".to_string()),
    }
}

fn ensure_native_supported(program: &Program) -> Result<(), String> {
    if !program.externs.is_empty() {
        return Err("native backend does not support C extern imports yet".to_string());
    }
    for f in &program.functions {
        for stmt in &f.body {
            match stmt {
                Stmt::Let { .. }
                | Stmt::Assign { .. }
                | Stmt::Return { .. }
                | Stmt::Expr { .. }
                | Stmt::IfIs { .. } => {}
                Stmt::If {
                    then_body,
                    elif_arms,
                    else_body,
                    ..
                } => {
                    for stmt in then_body {
                        ensure_stmt_native_supported(stmt)?;
                    }
                    for arm in elif_arms {
                        for stmt in &arm.body {
                            ensure_stmt_native_supported(stmt)?;
                        }
                    }
                    for stmt in else_body {
                        ensure_stmt_native_supported(stmt)?;
                    }
                }
                Stmt::While { body, .. } => {
                    for stmt in body {
                        ensure_stmt_native_supported(stmt)?;
                    }
                }
                Stmt::ThreadWhile { .. } | Stmt::ThreadCall { .. } => {
                    return Err(
                        "native backend does not support thread() syntax yet; interpreter fallback required"
                            .to_string(),
                    );
                }
                Stmt::ForRange { body, .. } => {
                    for stmt in body {
                        ensure_stmt_native_supported(stmt)?;
                    }
                }
                Stmt::Comptime { body, .. } => {
                    for stmt in body {
                        ensure_stmt_native_supported(stmt)?;
                    }
                }
                Stmt::Pass { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
            }
        }
    }
    Ok(())
}

fn ensure_stmt_native_supported(stmt: &Stmt) -> Result<(), String> {
    match stmt {
        Stmt::Let { .. } | Stmt::Assign { .. } | Stmt::Return { .. } | Stmt::Expr { .. } => {}
        Stmt::IfIs { value: _, arms, .. } => {
            for arm in arms {
                for stmt in &arm.body {
                    ensure_stmt_native_supported(stmt)?;
                }
            }
        }
        Stmt::If {
            then_body,
            elif_arms,
            else_body,
            ..
        } => {
            for stmt in then_body {
                ensure_stmt_native_supported(stmt)?;
            }
            for arm in elif_arms {
                for stmt in &arm.body {
                    ensure_stmt_native_supported(stmt)?;
                }
            }
            for stmt in else_body {
                ensure_stmt_native_supported(stmt)?;
            }
        }
        Stmt::While { body, .. } => {
            for stmt in body {
                ensure_stmt_native_supported(stmt)?;
            }
        }
        Stmt::ThreadWhile { .. } | Stmt::ThreadCall { .. } => {
            return Err(
                "native backend does not support thread() syntax yet; interpreter fallback required"
                    .to_string(),
            );
        }
        Stmt::ForRange { body, .. } => {
            for stmt in body {
                ensure_stmt_native_supported(stmt)?;
            }
        }
        Stmt::Comptime { body, .. } => {
            for stmt in body {
                ensure_stmt_native_supported(stmt)?;
            }
        }
        Stmt::Pass { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
    Ok(())
}

fn hash_signature(params: &[crate::ast::Param], ret: &Type) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for p in params {
        p.ty.hash(&mut hasher);
    }
    ret.hash(&mut hasher);
    hasher.finish()
}

fn hash_body(body: &[Stmt]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    hasher.finish()
}

fn reset_native_runtime() {
    NATIVE_STRINGS.with(|pool| pool.borrow_mut().reset());
}

fn intern_native_string(value: &str) -> i64 {
    NATIVE_STRINGS.with(|pool| pool.borrow_mut().intern(value))
}

fn native_string(handle: i64) -> String {
    NATIVE_STRINGS.with(|pool| {
        pool.borrow()
            .by_handle
            .get(&handle)
            .cloned()
            .unwrap_or_else(|| handle.to_string())
    })
}

fn register_runtime_symbols(builder: &mut JITBuilder) {
    builder.symbol("__xlang_str_truthy", __xlang_str_truthy as *const u8);
    builder.symbol("__xlang_str_concat", __xlang_str_concat as *const u8);
    builder.symbol("__xlang_str_eq", __xlang_str_eq as *const u8);
    builder.symbol("__xlang_str_len", __xlang_str_len as *const u8);
    builder.symbol("__xlang_str_contains", __xlang_str_contains as *const u8);
    builder.symbol("__xlang_str_find", __xlang_str_find as *const u8);
    builder.symbol(
        "__xlang_str_starts_with",
        __xlang_str_starts_with as *const u8,
    );
    builder.symbol("__xlang_str_ends_with", __xlang_str_ends_with as *const u8);
    builder.symbol("__xlang_str_replace", __xlang_str_replace as *const u8);
    builder.symbol("__xlang_str_split_n", __xlang_str_split_n as *const u8);
    builder.symbol("__xlang_str_join", __xlang_str_join as *const u8);
    builder.symbol("__xlang_str_trim", __xlang_str_trim as *const u8);
    builder.symbol("__xlang_str_upper", __xlang_str_upper as *const u8);
    builder.symbol("__xlang_str_lower", __xlang_str_lower as *const u8);
    builder.symbol("__xlang_ord_str", __xlang_ord_str as *const u8);
    builder.symbol("__xlang_chr_i64", __xlang_chr_i64 as *const u8);
    builder.symbol("__xlang_ptr_can_read", __xlang_ptr_can_read as *const u8);
    builder.symbol("__xlang_ptr_can_write", __xlang_ptr_can_write as *const u8);
    builder.symbol("__xlang_ptr_read8", __xlang_ptr_read8 as *const u8);
    builder.symbol("__xlang_ptr_write8", __xlang_ptr_write8 as *const u8);
    builder.symbol("__xlang_ptr_read64", __xlang_ptr_read64 as *const u8);
    builder.symbol("__xlang_ptr_write64", __xlang_ptr_write64 as *const u8);
    builder.symbol("__xlang_to_str_i64", __xlang_to_str_i64 as *const u8);
    builder.symbol("__xlang_to_str_bool", __xlang_to_str_bool as *const u8);
    builder.symbol("__xlang_to_int_str", __xlang_to_int_str as *const u8);
    builder.symbol("__xlang_ct_hash_str", __xlang_ct_hash_str as *const u8);
    builder.symbol("__xlang_ct_xor_str", __xlang_ct_xor_str as *const u8);
    builder.symbol("__xlang_xor_decode", __xlang_xor_decode as *const u8);
    builder.symbol("__xlang_print_i64", __xlang_print_i64 as *const u8);
    builder.symbol("__xlang_print_bool", __xlang_print_bool as *const u8);
    builder.symbol("__xlang_print_str", __xlang_print_str as *const u8);
    builder.symbol("__xlang_print_space", __xlang_print_space as *const u8);
    builder.symbol("__xlang_print_newline", __xlang_print_newline as *const u8);
    builder.symbol("__xlang_stdout_flush", __xlang_stdout_flush as *const u8);
    builder.symbol("__xlang_input0", __xlang_input0 as *const u8);
    builder.symbol("__xlang_input1", __xlang_input1 as *const u8);
    builder.symbol("__xlang_argc", __xlang_argc as *const u8);
    builder.symbol("__xlang_argv", __xlang_argv as *const u8);
    builder.symbol("__xlang_sleep_ms", __xlang_sleep_ms as *const u8);
    builder.symbol("__xlang_clock_ms", __xlang_clock_ms as *const u8);
    builder.symbol("__xlang_assert", __xlang_assert as *const u8);
    builder.symbol("__xlang_panic_i64", __xlang_panic_i64 as *const u8);
    builder.symbol("__xlang_panic_bool", __xlang_panic_bool as *const u8);
    builder.symbol("__xlang_panic_str", __xlang_panic_str as *const u8);
    builder.symbol("__xlang_abs_i64", __xlang_abs_i64 as *const u8);
    builder.symbol("__xlang_pow_i64", __xlang_pow_i64 as *const u8);
    builder.symbol("__xlang_clamp_i64", __xlang_clamp_i64 as *const u8);
}

fn to_string_err(err: impl std::fmt::Display) -> String {
    err.to_string()
}

extern "C" fn __xlang_str_truthy(v: i64) -> i64 {
    if native_string(v).is_empty() {
        0
    } else {
        1
    }
}

extern "C" fn __xlang_str_concat(a: i64, b: i64) -> i64 {
    intern_native_string(&(native_string(a) + &native_string(b)))
}

extern "C" fn __xlang_str_eq(a: i64, b: i64) -> i64 {
    if native_string(a) == native_string(b) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_str_len(v: i64) -> i64 {
    native_string(v).chars().count() as i64
}

extern "C" fn __xlang_str_contains(a: i64, b: i64) -> i64 {
    if native_string(a).contains(&native_string(b)) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_str_find(a: i64, b: i64) -> i64 {
    native_string(a)
        .find(&native_string(b))
        .map(|i| i as i64)
        .unwrap_or(-1)
}

extern "C" fn __xlang_str_starts_with(a: i64, b: i64) -> i64 {
    if native_string(a).starts_with(&native_string(b)) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_str_ends_with(a: i64, b: i64) -> i64 {
    if native_string(a).ends_with(&native_string(b)) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_str_replace(a: i64, b: i64, c: i64) -> i64 {
    let out = native_string(a).replace(&native_string(b), &native_string(c));
    intern_native_string(&out)
}

extern "C" fn __xlang_str_split_n(s: i64, sep: i64, idx: i64) -> i64 {
    if idx < 0 {
        return intern_native_string("");
    }
    let out = native_string(s);
    let sep_s = native_string(sep);
    let part = out.split(&sep_s).nth(idx as usize).unwrap_or_default();
    intern_native_string(part)
}

extern "C" fn __xlang_str_join(a: i64, b: i64, sep: i64) -> i64 {
    let out = format!("{}{}{}", native_string(a), native_string(sep), native_string(b));
    intern_native_string(&out)
}

extern "C" fn __xlang_str_trim(a: i64) -> i64 {
    intern_native_string(native_string(a).trim())
}

extern "C" fn __xlang_str_upper(a: i64) -> i64 {
    let out = native_string(a).to_ascii_uppercase();
    intern_native_string(&out)
}

extern "C" fn __xlang_str_lower(a: i64) -> i64 {
    let out = native_string(a).to_ascii_lowercase();
    intern_native_string(&out)
}

extern "C" fn __xlang_ord_str(v: i64) -> i64 {
    native_string(v)
        .chars()
        .next()
        .map(|c| c as i64)
        .unwrap_or(0)
}

extern "C" fn __xlang_chr_i64(v: i64) -> i64 {
    let out = u32::try_from(v)
        .ok()
        .and_then(char::from_u32)
        .map(|c| c.to_string())
        .unwrap_or_default();
    intern_native_string(&out)
}

extern "C" fn __xlang_ptr_can_read(addr: i64, len: i64) -> i64 {
    if memory::can_read(addr, len) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_ptr_can_write(addr: i64, len: i64) -> i64 {
    if memory::can_write(addr, len) {
        1
    } else {
        0
    }
}

extern "C" fn __xlang_ptr_read8(addr: i64) -> i64 {
    memory::read8(addr).unwrap_or(0)
}

extern "C" fn __xlang_ptr_write8(addr: i64, value: i64) -> i64 {
    if memory::write8(addr, value) {
        0
    } else {
        -1
    }
}

extern "C" fn __xlang_ptr_read64(addr: i64) -> i64 {
    memory::read64(addr).unwrap_or(0)
}

extern "C" fn __xlang_ptr_write64(addr: i64, value: i64) -> i64 {
    if memory::write64(addr, value) {
        0
    } else {
        -1
    }
}

extern "C" fn __xlang_to_str_i64(v: i64) -> i64 {
    intern_native_string(&v.to_string())
}

extern "C" fn __xlang_to_str_bool(v: i64) -> i64 {
    intern_native_string(if v == 0 { "false" } else { "true" })
}

extern "C" fn __xlang_to_int_str(v: i64) -> i64 {
    let s = native_string(v);
    s.parse::<i64>()
        .unwrap_or_else(|_| panic!("cannot parse i64 from '{s}'"))
}

extern "C" fn __xlang_ct_hash_str(v: i64) -> i64 {
    builtins::ct_hash_str(&native_string(v))
}

extern "C" fn __xlang_ct_xor_str(v: i64, key: i64) -> i64 {
    let encoded = builtins::ct_xor_hex(&native_string(v), key);
    intern_native_string(&encoded)
}

extern "C" fn __xlang_xor_decode(v: i64, key: i64) -> i64 {
    let decoded =
        builtins::xor_decode_hex(&native_string(v), key).unwrap_or_else(|e| panic!("{e}"));
    intern_native_string(&decoded)
}

extern "C" fn __xlang_print_i64(v: i64) -> i64 {
    print!("{v}");
    0
}

extern "C" fn __xlang_print_bool(v: i64) -> i64 {
    if v == 0 {
        print!("false");
    } else {
        print!("true");
    }
    0
}

extern "C" fn __xlang_print_str(v: i64) -> i64 {
    print!("{}", native_string(v));
    0
}

extern "C" fn __xlang_print_space() -> i64 {
    print!(" ");
    0
}

extern "C" fn __xlang_print_newline() -> i64 {
    println!();
    0
}

extern "C" fn __xlang_stdout_flush() -> i64 {
    let _ = io::stdout().flush();
    0
}

fn read_line_trimmed(prompt: Option<&str>) -> String {
    if let Some(p) = prompt {
        print!("{p}");
        let _ = io::stdout().flush();
    }
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    line
}

extern "C" fn __xlang_input0() -> i64 {
    let line = read_line_trimmed(None);
    intern_native_string(&line)
}

extern "C" fn __xlang_input1(prompt: i64) -> i64 {
    let p = native_string(prompt);
    let line = read_line_trimmed(Some(&p));
    intern_native_string(&line)
}

extern "C" fn __xlang_argc() -> i64 {
    runtime_args::argc()
}

extern "C" fn __xlang_argv(index: i64) -> i64 {
    let value = runtime_args::argv(index);
    intern_native_string(&value)
}

extern "C" fn __xlang_sleep_ms(ms: i64) -> i64 {
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms as u64));
    }
    0
}

extern "C" fn __xlang_clock_ms() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards");
    now.as_millis() as i64
}

extern "C" fn __xlang_assert(v: i64) -> i64 {
    assert!(v != 0, "assertion failed");
    0
}

extern "C" fn __xlang_panic_i64(v: i64) -> i64 {
    panic!("panic: {v}");
}

extern "C" fn __xlang_panic_bool(v: i64) -> i64 {
    panic!("panic: {}", if v == 0 { "false" } else { "true" });
}

extern "C" fn __xlang_panic_str(v: i64) -> i64 {
    panic!("panic: {}", native_string(v));
}

extern "C" fn __xlang_abs_i64(v: i64) -> i64 {
    v.abs()
}

extern "C" fn __xlang_pow_i64(base: i64, exp: i64) -> i64 {
    assert!(exp >= 0, "builtin 'pow' expects a non-negative exponent");
    base.pow(exp as u32)
}

extern "C" fn __xlang_clamp_i64(v: i64, lo: i64, hi: i64) -> i64 {
    v.clamp(lo, hi)
}

unsafe extern "C" fn memcpy_shim(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    std::ptr::copy_nonoverlapping(src, dst, n);
    dst
}

unsafe extern "C" fn memset_shim(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    std::ptr::write_bytes(dst, c as u8, n);
    dst
}

unsafe extern "C" fn memmove_shim(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    std::ptr::copy(src, dst, n);
    dst
}
