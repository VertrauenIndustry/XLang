use std::collections::HashMap;

use rayon::prelude::*;

use crate::ast::Function;
use crate::ast::{BinOp, Expr, IsPattern, Program, Stmt, Type};
use crate::builtins;
use crate::diag::Diagnostic;
use crate::typecheck::Signature;

#[derive(Debug, Clone, PartialEq, Eq)]
struct VarState {
    ty: Type,
    moved: bool,
}

pub fn borrow_check(
    program: &Program,
    signatures: &HashMap<String, Signature>,
) -> Result<(), Vec<Diagnostic>> {
    let errors = program
        .functions
        .par_iter()
        .map(|f| borrow_check_function(f, signatures))
        .reduce(Vec::new, |mut acc, mut part| {
            acc.append(&mut part);
            acc
        });

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn borrow_check_function(f: &Function, signatures: &HashMap<String, Signature>) -> Vec<Diagnostic> {
    let mut errors = Vec::new();
    let mut vars: HashMap<String, VarState> = HashMap::new();
    let params = signatures
        .get(&f.name)
        .map(|s| s.params.clone())
        .unwrap_or_else(|| f.params.iter().map(|p| p.ty.clone()).collect());
    for (idx, p) in f.params.iter().enumerate() {
        let ty = params.get(idx).cloned().unwrap_or_else(|| p.ty.clone());
        vars.insert(p.name.clone(), VarState { ty, moved: false });
    }

    check_block(&f.body, &mut vars, signatures, &mut errors, 0);
    errors
}

fn check_block(
    block: &[Stmt],
    vars: &mut HashMap<String, VarState>,
    signatures: &HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
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
                check_expr(expr, true, *line, vars, signatures, errors);
                let inferred = infer_expr_type(expr, vars, signatures).unwrap_or(Type::Infer);
                vars.insert(
                    name.clone(),
                    VarState {
                        ty: if *ty == Type::Infer {
                            inferred
                        } else {
                            ty.clone()
                        },
                        moved: false,
                    },
                );
            }
            Stmt::Assign { name, expr, line } => {
                check_expr(expr, true, *line, vars, signatures, errors);
                let inferred = infer_expr_type(expr, vars, signatures).unwrap_or(Type::Infer);
                if let Some(state) = vars.get_mut(name) {
                    // Assignment rematerializes the binding after a move.
                    state.moved = false;
                } else {
                    vars.insert(
                        name.clone(),
                        VarState {
                            ty: inferred,
                            moved: false,
                        },
                    );
                }
            }
            Stmt::Return { expr, line } => {
                check_expr(expr, true, *line, vars, signatures, errors);
            }
            Stmt::Expr { expr, line } => {
                check_expr(expr, true, *line, vars, signatures, errors);
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                line,
            } => {
                check_expr(value, false, *line, vars, signatures, errors);
                for arm in arms {
                    for pattern in &arm.patterns {
                        check_is_pattern_expr(pattern, arm.line, vars, signatures, errors);
                    }
                    let mut scoped = vars.clone();
                    check_block(&arm.body, &mut scoped, signatures, errors, loop_depth);
                }
                if !else_body.is_empty() {
                    let mut scoped = vars.clone();
                    check_block(else_body, &mut scoped, signatures, errors, loop_depth);
                }
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                line,
            } => {
                check_expr(condition, false, *line, vars, signatures, errors);
                let mut then_scoped = vars.clone();
                check_block(then_body, &mut then_scoped, signatures, errors, loop_depth);
                for arm in elif_arms {
                    check_expr(&arm.condition, false, arm.line, vars, signatures, errors);
                    let mut scoped = vars.clone();
                    check_block(&arm.body, &mut scoped, signatures, errors, loop_depth);
                }
                if !else_body.is_empty() {
                    let mut scoped = vars.clone();
                    check_block(else_body, &mut scoped, signatures, errors, loop_depth);
                }
            }
            Stmt::While {
                condition,
                body,
                line,
            } => {
                check_expr(condition, false, *line, vars, signatures, errors);
                let mut scoped = vars.clone();
                check_block(body, &mut scoped, signatures, errors, loop_depth + 1);
            }
            Stmt::ThreadWhile {
                condition,
                body,
                count,
                wait: _,
                line,
            } => {
                check_expr(condition, false, *line, vars, signatures, errors);
                check_expr(count, false, *line, vars, signatures, errors);
                let mut scoped = vars.clone();
                check_block(body, &mut scoped, signatures, errors, loop_depth + 1);
            }
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                line,
            } => {
                check_expr(start, false, *line, vars, signatures, errors);
                check_expr(end, false, *line, vars, signatures, errors);
                if let Some(step) = step {
                    check_expr(step, false, *line, vars, signatures, errors);
                }
                let mut scoped = vars.clone();
                scoped.insert(
                    var.clone(),
                    VarState {
                        ty: Type::I64,
                        moved: false,
                    },
                );
                check_block(body, &mut scoped, signatures, errors, loop_depth + 1);
            }
            Stmt::ThreadCall {
                call,
                count,
                wait: _,
                line,
            } => {
                check_expr(call, false, *line, vars, signatures, errors);
                check_expr(count, false, *line, vars, signatures, errors);
            }
            Stmt::Comptime { body, .. } => {
                let mut scoped = vars.clone();
                check_block(body, &mut scoped, signatures, errors, loop_depth);
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

fn check_is_pattern_expr(
    pattern: &IsPattern,
    line: usize,
    vars: &mut HashMap<String, VarState>,
    signatures: &HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    match pattern {
        IsPattern::Value(expr)
        | IsPattern::Ne(expr)
        | IsPattern::Lt(expr)
        | IsPattern::Le(expr)
        | IsPattern::Gt(expr)
        | IsPattern::Ge(expr)
        | IsPattern::StartsWith(expr)
        | IsPattern::EndsWith(expr)
        | IsPattern::Contains(expr) => check_expr(expr, false, line, vars, signatures, errors),
        IsPattern::Range { start, end } => {
            check_expr(start, false, line, vars, signatures, errors);
            check_expr(end, false, line, vars, signatures, errors);
        }
    }
}

fn check_expr(
    expr: &Expr,
    consume: bool,
    line: usize,
    vars: &mut HashMap<String, VarState>,
    signatures: &HashMap<String, Signature>,
    errors: &mut Vec<Diagnostic>,
) {
    match expr {
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) => {}
        Expr::Var(name) => {
            if let Some(var) = vars.get_mut(name) {
                if var.moved {
                    errors.push(Diagnostic::new(
                        line,
                        format!("use after move of variable '{name}'"),
                    ));
                    return;
                }
                if consume && !var.ty.is_copy() {
                    var.moved = true;
                }
            } else {
                errors.push(Diagnostic::new(
                    line,
                    format!("use of unknown variable '{name}'"),
                ));
            }
        }
        Expr::Move(name) => {
            if let Some(var) = vars.get_mut(name) {
                if var.moved {
                    errors.push(Diagnostic::new(
                        line,
                        format!("double move of variable '{name}'"),
                    ));
                } else {
                    var.moved = true;
                }
            } else {
                errors.push(Diagnostic::new(
                    line,
                    format!("move of unknown variable '{name}'"),
                ));
            }
        }
        Expr::Unary { expr, .. } => {
            check_expr(expr, false, line, vars, signatures, errors);
        }
        Expr::Binary { left, right, .. } => {
            check_expr(left, false, line, vars, signatures, errors);
            check_expr(right, false, line, vars, signatures, errors);
        }
        Expr::Call { name, args } => {
            if let Some(sig) = signatures.get(name) {
                for (idx, arg) in args.iter().enumerate() {
                    let consume_arg = sig.params.get(idx).map(|t| !t.is_copy()).unwrap_or(true);
                    check_expr(arg, consume_arg, line, vars, signatures, errors);
                }
            } else if builtins::is_builtin(name) {
                for arg in args {
                    check_expr(arg, false, line, vars, signatures, errors);
                }
            } else {
                errors.push(Diagnostic::new(line, format!("unknown function '{name}'")));
            }
        }
    }
}

fn infer_expr_type(
    expr: &Expr,
    vars: &HashMap<String, VarState>,
    signatures: &HashMap<String, Signature>,
) -> Option<Type> {
    match expr {
        Expr::Int(_) => Some(Type::I64),
        Expr::Bool(_) => Some(Type::Bool),
        Expr::Str(_) => Some(Type::Str),
        Expr::Var(name) | Expr::Move(name) => vars.get(name).map(|v| v.ty.clone()),
        Expr::Unary { op, .. } => match op {
            crate::ast::UnaryOp::Neg => Some(Type::I64),
            crate::ast::UnaryOp::Not => Some(Type::Bool),
        },
        Expr::Binary { op, left, right } => match op {
            BinOp::Add => {
                let l = infer_expr_type(left, vars, signatures).unwrap_or(Type::Infer);
                let r = infer_expr_type(right, vars, signatures).unwrap_or(Type::Infer);
                if l == Type::Str && r == Type::Str {
                    Some(Type::Str)
                } else {
                    Some(Type::I64)
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => Some(Type::I64),
            BinOp::Or
            | BinOp::And
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge => Some(Type::Bool),
        },
        Expr::Call { name, args: _ } => {
            if let Some(sig) = signatures.get(name) {
                Some(sig.ret.clone())
            } else if builtins::is_builtin(name) {
                match name.as_str() {
                    "len" | "clock_ms" | "assert" | "panic" | "max" | "min" | "abs" | "int"
                    | "pow" | "clamp" | "ptr_read8" | "ptr_write8" | "ptr_read64"
                    | "ptr_write64" | "argc" | "sleep_ms" | "ord" => Some(Type::I64),
                    "contains" | "starts_with" | "ends_with" | "ptr_can_read" | "ptr_can_write" => {
                        Some(Type::Bool)
                    }
                    "str" | "replace" | "trim" | "upper" | "lower" | "input" | "argv" | "chr" => {
                        Some(Type::Str)
                    }
                    "bool" => Some(Type::Bool),
                    "print" | "write" | "println" => Some(Type::I64),
                    _ => Some(Type::Infer),
                }
            } else {
                Some(Type::Infer)
            }
        }
    }
}
