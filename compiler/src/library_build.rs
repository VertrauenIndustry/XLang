use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::{BinOp, Expr, IsPattern, Program, Stmt, Type, UnaryOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Dll,
    StaticLib,
}

pub fn build_library(program: &Program, output: &Path, kind: LibraryKind) -> Result<(), String> {
    let source = emit_rust_library_source(program)?;
    let mut tmp_path = PathBuf::from(output);
    tmp_path.set_extension("xlib.rs");
    fs::write(&tmp_path, source).map_err(|e| format!("failed writing temp source: {e}"))?;

    let crate_type = match kind {
        LibraryKind::Dll => "cdylib",
        LibraryKind::StaticLib => "staticlib",
    };
    let crate_name = sanitize_crate_name(output);

    let output_str = output
        .to_str()
        .ok_or_else(|| "output path must be valid UTF-8".to_string())?;
    let tmp_str = tmp_path
        .to_str()
        .ok_or_else(|| "temp path must be valid UTF-8".to_string())?;

    let rustc = Command::new("rustc")
        .arg("--crate-name")
        .arg(crate_name)
        .arg("--crate-type")
        .arg(crate_type)
        .arg(tmp_str)
        .arg("-O")
        .arg("-o")
        .arg(output_str)
        .output()
        .map_err(|e| format!("failed invoking rustc: {e}"))?;

    if !rustc.status.success() {
        let stderr = String::from_utf8_lossy(&rustc.stderr).to_string();
        return Err(format!("rustc failed while building library:\n{stderr}"));
    }

    Ok(())
}

pub fn emit_rust_library_source(program: &Program) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("#![allow(unused_mut, unused_variables)]\n");

    if !program.externs.is_empty() {
        for ext in &program.externs {
            if ext.ret != Type::I64 || ext.params.iter().any(|p| p.ty != Type::I64) {
                return Err(format!(
                    "extern '{}' is unsupported for library emit; only i64 signatures are supported",
                    ext.name
                ));
            }
            let link_name = normalize_link_name(&ext.library);
            out.push_str(&format!("#[link(name = \"{link_name}\")]\n"));
            out.push_str("extern \"C\" {\n");
            out.push_str(&format!("    fn {}(", ext.name));
            for (idx, p) in ext.params.iter().enumerate() {
                if idx > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("{}: i64", p.name));
            }
            out.push_str(") -> i64;\n");
            out.push_str("}\n\n");
        }
    }

    for f in &program.functions {
        if !(f.ret == Type::I64 || f.ret == Type::Infer) {
            return Err(format!(
                "function '{}' has unsupported return type for library build",
                f.name
            ));
        }
        for p in &f.params {
            if !(p.ty == Type::I64 || p.ty == Type::Infer) {
                return Err(format!(
                    "function '{}' has unsupported parameter '{}' type for library build",
                    f.name, p.name
                ));
            }
        }
        out.push_str("#[no_mangle]\n");
        out.push_str(&format!("pub extern \"C\" fn {}(", f.name));
        for (idx, p) in f.params.iter().enumerate() {
            if idx > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}: i64", p.name));
        }
        out.push_str(") -> i64 {\n");
        let mut declared: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
        emit_block(&f.body, 1, &mut declared, &mut out)?;
        out.push_str("    0\n");
        out.push_str("}\n\n");
    }

    Ok(out)
}

fn emit_block(
    block: &[Stmt],
    indent: usize,
    declared: &mut HashSet<String>,
    out: &mut String,
) -> Result<(), String> {
    let pad = "    ".repeat(indent);
    for stmt in block {
        match stmt {
            Stmt::Let { name, expr, .. } => {
                let expr = emit_expr(expr)?;
                out.push_str(&format!("{pad}let mut {name}: i64 = {expr};\n"));
                declared.insert(name.clone());
            }
            Stmt::Assign { name, expr, .. } => {
                let expr = emit_expr(expr)?;
                if declared.contains(name) {
                    out.push_str(&format!("{pad}{name} = {expr};\n"));
                } else {
                    out.push_str(&format!("{pad}let mut {name}: i64 = {expr};\n"));
                    declared.insert(name.clone());
                }
            }
            Stmt::Return { expr, .. } => {
                let expr = emit_expr(expr)?;
                out.push_str(&format!("{pad}return {expr};\n"));
            }
            Stmt::Expr { expr, .. } => {
                let expr = emit_expr(expr)?;
                out.push_str(&format!("{pad}let _ = {expr};\n"));
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                line,
            } => {
                let switch = emit_expr(value)?;
                let switch_var = format!("__ifis_{}_{}", line, indent);
                out.push_str(&format!("{pad}let {switch_var}: i64 = {switch};\n"));
                let mut first = true;
                for arm in arms {
                    let cond = emit_if_is_arm_cond(&switch_var, &arm.patterns)?;
                    if first {
                        out.push_str(&format!("{pad}if {cond} {{\n"));
                        first = false;
                    } else {
                        out.push_str(&format!("{pad}else if {cond} {{\n"));
                    }
                    let mut scoped = declared.clone();
                    emit_block(&arm.body, indent + 1, &mut scoped, out)?;
                    out.push_str(&format!("{pad}}}\n"));
                }
                if !else_body.is_empty() {
                    if first {
                        out.push_str(&format!("{pad}{{\n"));
                    } else {
                        out.push_str(&format!("{pad}else {{\n"));
                    }
                    let mut scoped = declared.clone();
                    emit_block(else_body, indent + 1, &mut scoped, out)?;
                    out.push_str(&format!("{pad}}}\n"));
                }
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                ..
            } => {
                let cond = emit_truthy(condition)?;
                out.push_str(&format!("{pad}if {cond} {{\n"));
                let mut then_scope = declared.clone();
                emit_block(then_body, indent + 1, &mut then_scope, out)?;
                out.push_str(&format!("{pad}}}"));
                for arm in elif_arms {
                    let cond = emit_truthy(&arm.condition)?;
                    out.push_str(&format!(" else if {cond} {{\n"));
                    let mut scoped = declared.clone();
                    emit_block(&arm.body, indent + 1, &mut scoped, out)?;
                    out.push_str(&format!("{pad}}}"));
                }
                if !else_body.is_empty() {
                    out.push_str(" else {\n");
                    let mut scoped = declared.clone();
                    emit_block(else_body, indent + 1, &mut scoped, out)?;
                    out.push_str(&format!("{pad}}}"));
                }
                out.push('\n');
            }
            Stmt::While {
                condition, body, ..
            } => {
                let cond = emit_truthy(condition)?;
                out.push_str(&format!("{pad}while {cond} {{\n"));
                let mut scoped = declared.clone();
                emit_block(body, indent + 1, &mut scoped, out)?;
                out.push_str(&format!("{pad}}}\n"));
            }
            Stmt::ThreadWhile { line, .. } | Stmt::ThreadCall { line, .. } => {
                return Err(format!(
                    "thread() syntax is unsupported in library build source emission (line {line})"
                ));
            }
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                ..
            } => {
                let start = emit_expr(start)?;
                let end = emit_expr(end)?;
                if let Some(step) = step {
                    let step = emit_expr(step)?;
                    out.push_str(&format!("{pad}let mut {var}: i64 = {start};\n"));
                    out.push_str(&format!("{pad}let __step_{var}: i64 = {step};\n"));
                    out.push_str(&format!("{pad}if __step_{var} > 0 {{\n"));
                    out.push_str(&format!("{pad}    while {var} < {end} {{\n"));
                    let mut scoped = declared.clone();
                    scoped.insert(var.clone());
                    emit_block(body, indent + 2, &mut scoped, out)?;
                    out.push_str(&format!("{pad}        {var} = {var} + __step_{var};\n"));
                    out.push_str(&format!("{pad}    }}\n"));
                    out.push_str(&format!("{pad}}} else if __step_{var} < 0 {{\n"));
                    out.push_str(&format!("{pad}    while {var} > {end} {{\n"));
                    let mut scoped = declared.clone();
                    scoped.insert(var.clone());
                    emit_block(body, indent + 2, &mut scoped, out)?;
                    out.push_str(&format!("{pad}        {var} = {var} + __step_{var};\n"));
                    out.push_str(&format!("{pad}    }}\n"));
                    out.push_str(&format!("{pad}}}\n"));
                    continue;
                }
                out.push_str(&format!("{pad}for {var} in {start}..{end} {{\n"));
                let mut scoped = declared.clone();
                scoped.insert(var.clone());
                emit_block(body, indent + 1, &mut scoped, out)?;
                out.push_str(&format!("{pad}}}\n"));
            }
            Stmt::Comptime { body, .. } => {
                emit_block(body, indent, declared, out)?;
            }
            Stmt::Pass { .. } => {
                out.push_str(&format!("{pad};\n"));
            }
            Stmt::Break { .. } => {
                out.push_str(&format!("{pad}break;\n"));
            }
            Stmt::Continue { .. } => {
                out.push_str(&format!("{pad}continue;\n"));
            }
        }
    }
    Ok(())
}

fn emit_if_is_arm_cond(switch_var: &str, patterns: &[IsPattern]) -> Result<String, String> {
    let mut parts = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        parts.push(emit_is_pattern_cond(switch_var, pattern)?);
    }
    Ok(format!("({})", parts.join(" || ")))
}

fn emit_is_pattern_cond(switch_var: &str, pattern: &IsPattern) -> Result<String, String> {
    match pattern {
        IsPattern::Value(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) == ({v})"))
        }
        IsPattern::Ne(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) != ({v})"))
        }
        IsPattern::Lt(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) < ({v})"))
        }
        IsPattern::Le(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) <= ({v})"))
        }
        IsPattern::Gt(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) > ({v})"))
        }
        IsPattern::Ge(expr) => {
            let v = emit_expr(expr)?;
            Ok(format!("({switch_var}) >= ({v})"))
        }
        IsPattern::Range { start, end } => {
            let s = emit_expr(start)?;
            let e = emit_expr(end)?;
            Ok(format!("({switch_var}) >= ({s}) && ({switch_var}) < ({e})"))
        }
        IsPattern::StartsWith(_) | IsPattern::EndsWith(_) | IsPattern::Contains(_) => Err(
            "library build currently does not support string predicates in `if ... is ...`"
                .to_string(),
        ),
    }
}

fn emit_truthy(expr: &Expr) -> Result<String, String> {
    let value = emit_expr(expr)?;
    Ok(format!("({value}) != 0"))
}

fn emit_expr(expr: &Expr) -> Result<String, String> {
    match expr {
        Expr::Int(v) => Ok(v.to_string()),
        Expr::Bool(v) => Ok(if *v { "1".to_string() } else { "0".to_string() }),
        Expr::Str(_) => Err("library build does not support string expressions yet".to_string()),
        Expr::Var(name) | Expr::Move(name) => Ok(name.clone()),
        Expr::Unary { op, expr } => {
            let inner = emit_expr(expr)?;
            match op {
                UnaryOp::Neg => Ok(format!("(-({inner}))")),
                UnaryOp::Not => Ok(format!("((({inner}) == 0) as i64)")),
            }
        }
        Expr::Binary { op, left, right } => {
            let l = emit_expr(left)?;
            let r = emit_expr(right)?;
            match op {
                BinOp::Add => Ok(format!("({l} + {r})")),
                BinOp::Sub => Ok(format!("({l} - {r})")),
                BinOp::Mul => Ok(format!("({l} * {r})")),
                BinOp::Div => Ok(format!("({l} / {r})")),
                BinOp::Mod => Ok(format!("({l} % {r})")),
                BinOp::Eq => Ok(format!("((({l}) == ({r})) as i64)")),
                BinOp::Ne => Ok(format!("((({l}) != ({r})) as i64)")),
                BinOp::Lt => Ok(format!("((({l}) < ({r})) as i64)")),
                BinOp::Le => Ok(format!("((({l}) <= ({r})) as i64)")),
                BinOp::Gt => Ok(format!("((({l}) > ({r})) as i64)")),
                BinOp::Ge => Ok(format!("((({l}) >= ({r})) as i64)")),
                BinOp::And => Ok(format!("(((({l}) != 0) && (({r}) != 0)) as i64)")),
                BinOp::Or => Ok(format!("(((({l}) != 0) || (({r}) != 0)) as i64)")),
            }
        }
        Expr::Call { name, args } => {
            let mut rendered = Vec::with_capacity(args.len());
            for arg in args {
                rendered.push(emit_expr(arg)?);
            }
            match name.as_str() {
                "max" => {
                    if rendered.len() != 2 {
                        return Err("builtin 'max' expects 2 arguments".to_string());
                    }
                    Ok(format!("std::cmp::max({}, {})", rendered[0], rendered[1]))
                }
                "min" => {
                    if rendered.len() != 2 {
                        return Err("builtin 'min' expects 2 arguments".to_string());
                    }
                    Ok(format!("std::cmp::min({}, {})", rendered[0], rendered[1]))
                }
                "abs" => {
                    if rendered.len() != 1 {
                        return Err("builtin 'abs' expects 1 argument".to_string());
                    }
                    Ok(format!("({}).abs()", rendered[0]))
                }
                "pow" => {
                    if rendered.len() != 2 {
                        return Err("builtin 'pow' expects 2 arguments".to_string());
                    }
                    Ok(format!("({}).pow({} as u32)", rendered[0], rendered[1]))
                }
                "clamp" => {
                    if rendered.len() != 3 {
                        return Err("builtin 'clamp' expects 3 arguments".to_string());
                    }
                    Ok(format!(
                        "std::cmp::min(std::cmp::max({}, {}), {})",
                        rendered[0], rendered[1], rendered[2]
                    ))
                }
                "exit" => {
                    if rendered.len() != 1 {
                        return Err("builtin 'exit' expects 1 argument".to_string());
                    }
                    Ok(rendered[0].clone())
                }
                "print" | "write" | "println" | "input" | "argc" | "argv" | "sleep_ms"
                | "clock_ms" | "assert" | "panic" | "str" | "int" | "bool" | "ord" | "chr"
                | "len" | "contains" | "find" | "starts_with" | "ends_with" | "replace"
                | "split" | "join" | "trim" | "upper" | "lower" | "ptr_can_read"
                | "ptr_can_write" | "ptr_read8" | "ptr_write8"
                | "ptr_read64" | "ptr_write64" => Err(format!(
                    "builtin '{}' is unsupported in library build source emission",
                    name
                )),
                _ => Ok(format!("{name}({})", rendered.join(", "))),
            }
        }
    }
}

fn normalize_link_name(raw: &str) -> String {
    let without_ext = raw
        .strip_suffix(".dll")
        .or_else(|| raw.strip_suffix(".so"))
        .or_else(|| raw.strip_suffix(".dylib"))
        .unwrap_or(raw);
    without_ext
        .trim_start_matches("lib")
        .trim_end_matches(".lib")
        .to_string()
}

fn sanitize_crate_name(output: &Path) -> String {
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("xlib");
    let mut out = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "xlib".to_string()
    } else {
        out
    }
}
