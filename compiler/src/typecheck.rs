use std::collections::HashMap;

use rayon::prelude::*;

use crate::ast::{BinOp, Expr, ExternFunction, Function, IsPattern, Program, Stmt, Type, UnaryOp};
use crate::builtins;
use crate::diag::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub params: Vec<Type>,
    pub ret: Type,
}

struct FunctionCheckResult {
    name: String,
    errors: Vec<Diagnostic>,
    inferred_ret: Option<Type>,
}

pub fn type_check(program: &Program) -> Result<HashMap<String, Signature>, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut sigs: HashMap<String, Signature> = HashMap::new();

    for ext in &program.externs {
        register_extern(ext, &mut sigs, &mut errors);
    }
    for f in &program.functions {
        register_function(f, &mut sigs, &mut errors);
    }

    let results: Vec<FunctionCheckResult> = program
        .functions
        .par_iter()
        .map(|f| type_check_function(f, &sigs))
        .collect();

    for result in results {
        if let Some(ret) = result.inferred_ret {
            if let Some(sig) = sigs.get_mut(&result.name) {
                sig.ret = ret;
            }
        }
        errors.extend(result.errors);
    }

    let Some(main) = sigs.get("main") else {
        errors.push(Diagnostic::new(0, "entry function 'main' is required"));
        return Err(errors);
    };
    if !main.params.is_empty() {
        errors.push(Diagnostic::new(0, "main must not have parameters"));
    }
    if main.ret != Type::I64 {
        errors.push(Diagnostic::new(0, "main must return i64"));
    }

    if errors.is_empty() {
        Ok(sigs)
    } else {
        Err(errors)
    }
}

fn register_extern(
    ext: &ExternFunction,
    sigs: &mut HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    if sigs.contains_key(&ext.name) {
        errors.push(Diagnostic::new(
            ext.line,
            format!("duplicate function '{}'", ext.name),
        ));
        return;
    }
    let params: Vec<Type> = ext
        .params
        .iter()
        .map(|p| normalize_sig_type(&p.ty))
        .collect();
    let ret = normalize_sig_type(&ext.ret);
    if ret != Type::I64 || params.iter().any(|t| *t != Type::I64) {
        errors.push(Diagnostic::new(
            ext.line,
            "C extern support currently allows only i64 parameters and i64 return type",
        ));
    }
    sigs.insert(ext.name.clone(), Signature { params, ret });
}

fn register_function(
    f: &Function,
    sigs: &mut HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    if sigs.contains_key(&f.name) {
        errors.push(Diagnostic::new(
            f.line,
            format!("duplicate function '{}'", f.name),
        ));
        return;
    }
    sigs.insert(
        f.name.clone(),
        Signature {
            params: f.params.iter().map(|p| normalize_sig_type(&p.ty)).collect(),
            ret: normalize_ret_seed(&f.ret),
        },
    );
}

fn type_check_function(f: &Function, sigs: &HashMap<String, Signature>) -> FunctionCheckResult {
    let mut errors = Vec::new();
    let mut vars: HashMap<String, Type> = HashMap::new();
    let mut return_types: Vec<Type> = Vec::new();
    for p in &f.params {
        vars.insert(p.name.clone(), normalize_sig_type(&p.ty));
    }

    check_block(
        &f.body,
        &mut vars,
        sigs,
        &mut errors,
        &mut return_types,
        &f.ret,
        &f.name,
        0,
    );

    let inferred_ret = if f.ret == Type::Infer {
        infer_return_type(&f.name, &return_types, &mut errors)
    } else {
        None
    };

    FunctionCheckResult {
        name: f.name.clone(),
        errors,
        inferred_ret,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_block(
    block: &[Stmt],
    vars: &mut HashMap<String, Type>,
    sigs: &HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
    return_types: &mut Vec<Type>,
    declared_ret: &Type,
    function_name: &str,
    loop_depth: usize,
) {
    for stmt in block {
        match stmt {
            Stmt::Let {
                name,
                ty,
                expr,
                line,
            } => {
                let got = expr_type(expr, vars, sigs, *line, errors);
                if let Some(got) = got {
                    if *ty == Type::Infer {
                        vars.insert(name.clone(), got);
                    } else if types_compatible(ty, &got) {
                        vars.insert(name.clone(), normalize_sig_type(ty));
                    } else {
                        errors.push(Diagnostic::new(
                            *line,
                            format!(
                                "type mismatch for '{}': expected {:?}, got {:?}",
                                name, ty, got
                            ),
                        ));
                    }
                }
            }
            Stmt::Assign { name, expr, line } => {
                let got = expr_type(expr, vars, sigs, *line, errors);
                if let Some(got) = got {
                    match vars.get(name).cloned() {
                        Some(existing) => {
                            if !types_compatible(&existing, &got) {
                                errors.push(Diagnostic::new(
                                    *line,
                                    format!(
                                        "assignment type mismatch for '{}': expected {:?}, got {:?}",
                                        name, existing, got
                                    ),
                                ));
                            }
                        }
                        None => {
                            vars.insert(name.clone(), got);
                        }
                    }
                }
            }
            Stmt::Return { expr, line } => {
                let got = expr_type(expr, vars, sigs, *line, errors);
                if let Some(got) = got {
                    if *declared_ret == Type::Infer {
                        return_types.push(got);
                    } else if !types_compatible(declared_ret, &got) {
                        errors.push(Diagnostic::new(
                            *line,
                            format!(
                                "return type mismatch in '{}': expected {:?}, got {:?}",
                                function_name, declared_ret, got
                            ),
                        ));
                    }
                }
            }
            Stmt::Expr { expr, line } => {
                let _ = expr_type(expr, vars, sigs, *line, errors);
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                line,
            } => {
                let Some(value_ty) = expr_type(value, vars, sigs, *line, errors) else {
                    continue;
                };
                for arm in arms {
                    for pattern in &arm.patterns {
                        check_is_pattern_type(&value_ty, pattern, vars, sigs, arm.line, errors);
                    }
                    let mut scoped = vars.clone();
                    check_block(
                        &arm.body,
                        &mut scoped,
                        sigs,
                        errors,
                        return_types,
                        declared_ret,
                        function_name,
                        loop_depth,
                    );
                }
                if !else_body.is_empty() {
                    let mut scoped = vars.clone();
                    check_block(
                        else_body,
                        &mut scoped,
                        sigs,
                        errors,
                        return_types,
                        declared_ret,
                        function_name,
                        loop_depth,
                    );
                }
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                line,
            } => {
                if let Some(cond_ty) = expr_type(condition, vars, sigs, *line, errors) {
                    if !is_truthy_type(&cond_ty) {
                        errors.push(Diagnostic::new(
                            *line,
                            format!("if condition must be bool, i64, or str; got {:?}", cond_ty),
                        ));
                    }
                }

                let mut then_scoped = vars.clone();
                check_block(
                    then_body,
                    &mut then_scoped,
                    sigs,
                    errors,
                    return_types,
                    declared_ret,
                    function_name,
                    loop_depth,
                );

                for arm in elif_arms {
                    if let Some(cond_ty) = expr_type(&arm.condition, vars, sigs, arm.line, errors) {
                        if !is_truthy_type(&cond_ty) {
                            errors.push(Diagnostic::new(
                                arm.line,
                                format!(
                                    "elif condition must be bool, i64, or str; got {:?}",
                                    cond_ty
                                ),
                            ));
                        }
                    }
                    let mut scoped = vars.clone();
                    check_block(
                        &arm.body,
                        &mut scoped,
                        sigs,
                        errors,
                        return_types,
                        declared_ret,
                        function_name,
                        loop_depth,
                    );
                }

                if !else_body.is_empty() {
                    let mut scoped = vars.clone();
                    check_block(
                        else_body,
                        &mut scoped,
                        sigs,
                        errors,
                        return_types,
                        declared_ret,
                        function_name,
                        loop_depth,
                    );
                }
            }
            Stmt::While {
                condition,
                body,
                line,
            } => {
                if let Some(cond_ty) = expr_type(condition, vars, sigs, *line, errors) {
                    if !is_truthy_type(&cond_ty) {
                        errors.push(Diagnostic::new(
                            *line,
                            format!(
                                "while condition must be bool, i64, or str; got {:?}",
                                cond_ty
                            ),
                        ));
                    }
                }
                let mut scoped = vars.clone();
                check_block(
                    body,
                    &mut scoped,
                    sigs,
                    errors,
                    return_types,
                    declared_ret,
                    function_name,
                    loop_depth + 1,
                );
            }
            Stmt::ThreadWhile {
                condition,
                body,
                count,
                wait: _,
                line,
            } => {
                if let Some(cond_ty) = expr_type(condition, vars, sigs, *line, errors) {
                    if !is_truthy_type(&cond_ty) {
                        errors.push(Diagnostic::new(
                            *line,
                            format!(
                                "while condition must be bool, i64, or str; got {:?}",
                                cond_ty
                            ),
                        ));
                    }
                }
                if let Some(count_ty) = expr_type(count, vars, sigs, *line, errors) {
                    if normalize_sig_type(&count_ty) != Type::I64 {
                        errors.push(Diagnostic::new(
                            *line,
                            format!("thread count must be i64; got {:?}", count_ty),
                        ));
                    }
                }
                let mut scoped = vars.clone();
                check_block(
                    body,
                    &mut scoped,
                    sigs,
                    errors,
                    return_types,
                    declared_ret,
                    function_name,
                    loop_depth + 1,
                );
            }
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                line,
            } => {
                if let Some(start_ty) = expr_type(start, vars, sigs, *line, errors) {
                    if normalize_sig_type(&start_ty) != Type::I64 {
                        errors.push(Diagnostic::new(
                            *line,
                            format!("for-range start bound must be i64; got {:?}", start_ty),
                        ));
                    }
                }
                if let Some(end_ty) = expr_type(end, vars, sigs, *line, errors) {
                    if normalize_sig_type(&end_ty) != Type::I64 {
                        errors.push(Diagnostic::new(
                            *line,
                            format!("for-range end bound must be i64; got {:?}", end_ty),
                        ));
                    }
                }
                if let Some(step_expr) = step {
                    if let Some(step_ty) = expr_type(step_expr, vars, sigs, *line, errors) {
                        if normalize_sig_type(&step_ty) != Type::I64 {
                            errors.push(Diagnostic::new(
                                *line,
                                format!("for-range step must be i64; got {:?}", step_ty),
                            ));
                        }
                    }
                    if matches!(step_expr, Expr::Int(0)) {
                        errors.push(Diagnostic::new(*line, "for-range step cannot be 0"));
                    }
                }
                let mut scoped = vars.clone();
                scoped.insert(var.clone(), Type::I64);
                check_block(
                    body,
                    &mut scoped,
                    sigs,
                    errors,
                    return_types,
                    declared_ret,
                    function_name,
                    loop_depth + 1,
                );
            }
            Stmt::ThreadCall {
                call,
                count,
                wait: _,
                line,
            } => {
                if !matches!(call, Expr::Call { .. }) {
                    errors.push(Diagnostic::new(
                        *line,
                        "thread() currently supports only function call statements",
                    ));
                }
                let _ = expr_type(call, vars, sigs, *line, errors);
                if let Some(count_ty) = expr_type(count, vars, sigs, *line, errors) {
                    if normalize_sig_type(&count_ty) != Type::I64 {
                        errors.push(Diagnostic::new(
                            *line,
                            format!("thread count must be i64; got {:?}", count_ty),
                        ));
                    }
                }
            }
            Stmt::Comptime { body, .. } => {
                let mut scoped = vars.clone();
                check_block(
                    body,
                    &mut scoped,
                    sigs,
                    errors,
                    return_types,
                    declared_ret,
                    function_name,
                    loop_depth,
                );
            }
            Stmt::Break { line } => {
                if loop_depth == 0 {
                    errors.push(Diagnostic::new(
                        *line,
                        "`break` can only appear inside a loop",
                    ));
                }
            }
            Stmt::Continue { line } => {
                if loop_depth == 0 {
                    errors.push(Diagnostic::new(
                        *line,
                        "`continue` can only appear inside a loop",
                    ));
                }
            }
            Stmt::Pass { .. } => {}
        }
    }
}

fn check_is_pattern_type(
    value_ty: &Type,
    pattern: &IsPattern,
    vars: &HashMap<String, Type>,
    sigs: &HashMap<String, Signature>,
    line: usize,
    errors: &mut Vec<Diagnostic>,
) {
    let value_ty = normalize_sig_type(value_ty);
    match pattern {
        IsPattern::Value(expr) => {
            if let Some(pattern_ty) = expr_type(expr, vars, sigs, line, errors) {
                if !types_compatible(&value_ty, &pattern_ty) {
                    errors.push(Diagnostic::new(
                        line,
                        format!(
                            "`is` pattern type mismatch: switch value is {:?}, pattern is {:?}",
                            value_ty, pattern_ty
                        ),
                    ));
                }
            }
        }
        IsPattern::Ne(expr) => {
            if let Some(pattern_ty) = expr_type(expr, vars, sigs, line, errors) {
                let p = normalize_sig_type(&pattern_ty);
                if value_ty != p || !matches!(value_ty, Type::I64 | Type::Bool | Type::Str) {
                    errors.push(Diagnostic::new(
                        line,
                        "`is !=` requires matching i64/bool/str types",
                    ));
                }
            }
        }
        IsPattern::Lt(expr) | IsPattern::Le(expr) | IsPattern::Gt(expr) | IsPattern::Ge(expr) => {
            if value_ty != Type::I64 {
                errors.push(Diagnostic::new(
                    line,
                    "`is < <= > >=` patterns require an i64 switch value",
                ));
            }
            if let Some(pattern_ty) = expr_type(expr, vars, sigs, line, errors) {
                if normalize_sig_type(&pattern_ty) != Type::I64 {
                    errors.push(Diagnostic::new(
                        line,
                        "`is < <= > >=` patterns require i64 bounds",
                    ));
                }
            }
        }
        IsPattern::StartsWith(expr) | IsPattern::EndsWith(expr) | IsPattern::Contains(expr) => {
            if value_ty != Type::Str {
                errors.push(Diagnostic::new(
                    line,
                    "`is starts_with/ends_with/contains` requires a str switch value",
                ));
            }
            if let Some(pattern_ty) = expr_type(expr, vars, sigs, line, errors) {
                if normalize_sig_type(&pattern_ty) != Type::Str {
                    errors.push(Diagnostic::new(
                        line,
                        "`is starts_with/ends_with/contains` requires str patterns",
                    ));
                }
            }
        }
        IsPattern::Range { start, end } => {
            if value_ty != Type::I64 {
                errors.push(Diagnostic::new(
                    line,
                    "`is a..b` range patterns require an i64 switch value",
                ));
            }
            if let Some(start_ty) = expr_type(start, vars, sigs, line, errors) {
                if normalize_sig_type(&start_ty) != Type::I64 {
                    errors.push(Diagnostic::new(line, "`is a..b` range start must be i64"));
                }
            }
            if let Some(end_ty) = expr_type(end, vars, sigs, line, errors) {
                if normalize_sig_type(&end_ty) != Type::I64 {
                    errors.push(Diagnostic::new(line, "`is a..b` range end must be i64"));
                }
            }
        }
    }
}

fn infer_return_type(
    function_name: &str,
    return_types: &[Type],
    errors: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if return_types.is_empty() {
        return Some(Type::I64);
    }
    let first = return_types[0].clone();
    if return_types.iter().all(|t| types_compatible(&first, t)) {
        Some(normalize_sig_type(&first))
    } else {
        errors.push(Diagnostic::new(
            0,
            format!(
                "function '{}' has inconsistent return types; add an explicit '-> type'",
                function_name
            ),
        ));
        Some(Type::I64)
    }
}

fn expr_type(
    expr: &Expr,
    vars: &HashMap<String, Type>,
    sigs: &HashMap<String, Signature>,
    line: usize,
    errors: &mut Vec<Diagnostic>,
) -> Option<Type> {
    match expr {
        Expr::Int(_) => Some(Type::I64),
        Expr::Bool(_) => Some(Type::Bool),
        Expr::Str(_) => Some(Type::Str),
        Expr::Var(name) | Expr::Move(name) => vars.get(name).cloned().or_else(|| {
            errors.push(Diagnostic::new(
                line,
                format!("use of unknown variable '{name}'"),
            ));
            None
        }),
        Expr::Unary { op, expr } => {
            let v = normalize_sig_type(&expr_type(expr, vars, sigs, line, errors)?);
            match op {
                UnaryOp::Neg => {
                    if v != Type::I64 {
                        errors.push(Diagnostic::new(line, "unary '-' expects i64"));
                        None
                    } else {
                        Some(Type::I64)
                    }
                }
                UnaryOp::Not => {
                    if v != Type::Bool {
                        errors.push(Diagnostic::new(line, "unary 'not' expects bool"));
                        None
                    } else {
                        Some(Type::Bool)
                    }
                }
            }
        }
        Expr::Binary { op, left, right } => {
            let l = normalize_sig_type(&expr_type(left, vars, sigs, line, errors)?);
            let r = normalize_sig_type(&expr_type(right, vars, sigs, line, errors)?);
            match op {
                BinOp::Add => {
                    if l == Type::I64 && r == Type::I64 {
                        Some(Type::I64)
                    } else if l == Type::Str && r == Type::Str {
                        Some(Type::Str)
                    } else {
                        errors.push(Diagnostic::new(
                            line,
                            "operator '+' supports (i64+i64) or (str+str)",
                        ));
                        None
                    }
                }
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    if l != Type::I64 || r != Type::I64 {
                        errors.push(Diagnostic::new(
                            line,
                            "arithmetic operators -,*,/,% only support i64",
                        ));
                        None
                    } else {
                        Some(Type::I64)
                    }
                }
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    if l != Type::I64 || r != Type::I64 {
                        errors.push(Diagnostic::new(
                            line,
                            "comparison operators < <= > >= only support i64",
                        ));
                        None
                    } else {
                        Some(Type::Bool)
                    }
                }
                BinOp::Eq | BinOp::Ne => {
                    if l != r {
                        errors.push(Diagnostic::new(
                            line,
                            format!(
                                "equality operands must have matching types, got {:?} and {:?}",
                                l, r
                            ),
                        ));
                        None
                    } else if !matches!(l, Type::I64 | Type::Bool | Type::Str) {
                        errors.push(Diagnostic::new(
                            line,
                            "equality operators only support i64, bool, or str",
                        ));
                        None
                    } else {
                        Some(Type::Bool)
                    }
                }
                BinOp::And | BinOp::Or => {
                    if l != Type::Bool || r != Type::Bool {
                        errors.push(Diagnostic::new(
                            line,
                            "logical operators and/or require bool operands",
                        ));
                        None
                    } else {
                        Some(Type::Bool)
                    }
                }
            }
        }
        Expr::Call { name, args } => {
            let mut arg_types = Vec::with_capacity(args.len());
            for arg in args {
                let got = expr_type(arg, vars, sigs, line, errors);
                if let Some(got) = got {
                    arg_types.push(normalize_sig_type(&got));
                } else {
                    return None;
                }
            }

            if let Some(sig) = sigs.get(name) {
                if arg_types.len() != sig.params.len() {
                    errors.push(Diagnostic::new(
                        line,
                        format!(
                            "function '{}' expected {} args, got {}",
                            name,
                            sig.params.len(),
                            arg_types.len()
                        ),
                    ));
                    return None;
                }
                for (idx, (got, pty)) in arg_types.iter().zip(sig.params.iter()).enumerate() {
                    if !types_compatible(pty, got) {
                        errors.push(Diagnostic::new(
                            line,
                            format!(
                                "arg {} in call '{}' expected {:?}, got {:?}",
                                idx, name, pty, got
                            ),
                        ));
                    }
                }
                return Some(normalize_sig_type(&sig.ret));
            }

            match builtins::return_type(name, &arg_types) {
                Some(Ok(ret)) => Some(ret),
                Some(Err(msg)) => {
                    errors.push(Diagnostic::new(line, msg));
                    None
                }
                None => {
                    errors.push(Diagnostic::new(line, format!("unknown function '{name}'")));
                    None
                }
            }
        }
    }
}

fn normalize_sig_type(ty: &Type) -> Type {
    if *ty == Type::Infer {
        Type::I64
    } else {
        ty.clone()
    }
}

fn normalize_ret_seed(ty: &Type) -> Type {
    if *ty == Type::Infer {
        Type::Infer
    } else {
        ty.clone()
    }
}

fn types_compatible(expected: &Type, got: &Type) -> bool {
    let e = normalize_sig_type(expected);
    let g = normalize_sig_type(got);
    e == g
}

fn is_truthy_type(ty: &Type) -> bool {
    matches!(normalize_sig_type(ty), Type::Bool | Type::I64 | Type::Str)
}
