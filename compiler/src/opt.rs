use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

use crate::ast::{BinOp, Expr, Function, IsPattern, Program, Stmt};
use crate::builtins;

#[derive(Debug, Clone)]
struct InlineTemplate {
    params: Vec<String>,
    body: Expr,
}

#[derive(Debug, Clone)]
struct OptCtx {
    inlinable: HashMap<String, InlineTemplate>,
    max_inline_depth: u8,
}

pub fn optimize(program: &Program) -> Program {
    let ctx = OptCtx {
        inlinable: collect_inlinable_functions(program),
        max_inline_depth: 4,
    };
    Program {
        functions: program
            .functions
            .par_iter()
            .map(|f| opt_fn(f, &ctx))
            .collect(),
        externs: program.externs.clone(),
        imports: program.imports.clone(),
    }
}

fn collect_inlinable_functions(program: &Program) -> HashMap<String, InlineTemplate> {
    let mut out = HashMap::new();
    for f in &program.functions {
        let Some(Stmt::Return { expr, .. }) = f.body.first() else {
            continue;
        };
        if f.body.len() != 1 {
            continue;
        }
        let params = f.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        let allowed = params.iter().cloned().collect::<HashSet<_>>();
        if !expr_uses_only_params(expr, &allowed) {
            continue;
        }
        out.insert(
            f.name.clone(),
            InlineTemplate {
                params,
                body: expr.clone(),
            },
        );
    }
    out
}

fn opt_fn(f: &Function, ctx: &OptCtx) -> Function {
    let mut cloned = f.clone();
    cloned.body = opt_block(cloned.body, ctx, &f.name, 0);
    cloned
}

fn opt_block(block: Vec<Stmt>, ctx: &OptCtx, current_fn: &str, inline_depth: u8) -> Vec<Stmt> {
    block
        .into_iter()
        .map(|stmt| opt_stmt(stmt, ctx, current_fn, inline_depth))
        .collect()
}

fn opt_stmt(stmt: Stmt, ctx: &OptCtx, current_fn: &str, inline_depth: u8) -> Stmt {
    match stmt {
        Stmt::Let {
            name,
            ty,
            expr,
            line,
        } => Stmt::Let {
            name,
            ty,
            expr: fold_expr(expr, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::Assign { name, expr, line } => Stmt::Assign {
            name,
            expr: fold_expr(expr, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::Return { expr, line } => Stmt::Return {
            expr: fold_expr(expr, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::Expr { expr, line } => Stmt::Expr {
            expr: fold_expr(expr, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::IfIs {
            value,
            arms,
            else_body,
            line,
        } => Stmt::IfIs {
            value: fold_expr(value, ctx, current_fn, inline_depth),
            arms: arms
                .into_iter()
                .map(|arm| crate::ast::IfIsArm {
                    patterns: arm
                        .patterns
                        .into_iter()
                        .map(|p| fold_pattern(p, ctx, current_fn, inline_depth))
                        .collect(),
                    body: opt_block(arm.body, ctx, current_fn, inline_depth),
                    line: arm.line,
                })
                .collect(),
            else_body: opt_block(else_body, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::If {
            condition,
            then_body,
            elif_arms,
            else_body,
            line,
        } => Stmt::If {
            condition: fold_expr(condition, ctx, current_fn, inline_depth),
            then_body: opt_block(then_body, ctx, current_fn, inline_depth),
            elif_arms: elif_arms
                .into_iter()
                .map(|arm| crate::ast::ElifArm {
                    condition: fold_expr(arm.condition, ctx, current_fn, inline_depth),
                    body: opt_block(arm.body, ctx, current_fn, inline_depth),
                    line: arm.line,
                })
                .collect(),
            else_body: opt_block(else_body, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::While {
            condition,
            body,
            line,
        } => Stmt::While {
            condition: fold_expr(condition, ctx, current_fn, inline_depth),
            body: opt_block(body, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::ThreadWhile {
            condition,
            body,
            count,
            wait,
            line,
        } => Stmt::ThreadWhile {
            condition: fold_expr(condition, ctx, current_fn, inline_depth),
            body: opt_block(body, ctx, current_fn, inline_depth),
            count: fold_expr(count, ctx, current_fn, inline_depth),
            wait,
            line,
        },
        Stmt::ForRange {
            var,
            start,
            end,
            step,
            body,
            line,
        } => Stmt::ForRange {
            var,
            start: fold_expr(start, ctx, current_fn, inline_depth),
            end: fold_expr(end, ctx, current_fn, inline_depth),
            step: step.map(|s| fold_expr(s, ctx, current_fn, inline_depth)),
            body: opt_block(body, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::ThreadCall {
            call,
            count,
            wait,
            line,
        } => Stmt::ThreadCall {
            call: fold_expr(call, ctx, current_fn, inline_depth),
            count: fold_expr(count, ctx, current_fn, inline_depth),
            wait,
            line,
        },
        Stmt::Comptime { body, line } => Stmt::Comptime {
            body: opt_block(body, ctx, current_fn, inline_depth),
            line,
        },
        Stmt::Break { line } => Stmt::Break { line },
        Stmt::Continue { line } => Stmt::Continue { line },
        Stmt::Pass { line } => Stmt::Pass { line },
    }
}

fn fold_pattern(pattern: IsPattern, ctx: &OptCtx, current_fn: &str, inline_depth: u8) -> IsPattern {
    match pattern {
        IsPattern::Value(expr) => IsPattern::Value(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::Ne(expr) => IsPattern::Ne(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::Lt(expr) => IsPattern::Lt(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::Le(expr) => IsPattern::Le(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::Gt(expr) => IsPattern::Gt(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::Ge(expr) => IsPattern::Ge(fold_expr(expr, ctx, current_fn, inline_depth)),
        IsPattern::StartsWith(expr) => {
            IsPattern::StartsWith(fold_expr(expr, ctx, current_fn, inline_depth))
        }
        IsPattern::EndsWith(expr) => {
            IsPattern::EndsWith(fold_expr(expr, ctx, current_fn, inline_depth))
        }
        IsPattern::Contains(expr) => {
            IsPattern::Contains(fold_expr(expr, ctx, current_fn, inline_depth))
        }
        IsPattern::Range { start, end } => IsPattern::Range {
            start: fold_expr(start, ctx, current_fn, inline_depth),
            end: fold_expr(end, ctx, current_fn, inline_depth),
        },
    }
}

fn fold_expr(expr: Expr, ctx: &OptCtx, current_fn: &str, inline_depth: u8) -> Expr {
    match expr {
        Expr::Unary { op, expr } => {
            let inner = fold_expr(*expr, ctx, current_fn, inline_depth);
            match (op, &inner) {
                (crate::ast::UnaryOp::Neg, Expr::Int(v)) => Expr::Int(-v),
                (crate::ast::UnaryOp::Not, Expr::Bool(v)) => Expr::Bool(!v),
                _ => Expr::Unary {
                    op,
                    expr: Box::new(inner),
                },
            }
        }
        Expr::Binary { op, left, right } => {
            let left = fold_expr(*left, ctx, current_fn, inline_depth);
            let right = fold_expr(*right, ctx, current_fn, inline_depth);
            match (&left, &right, op) {
                (Expr::Bool(a), Expr::Bool(b), BinOp::And) => Expr::Bool(*a && *b),
                (Expr::Bool(a), Expr::Bool(b), BinOp::Or) => Expr::Bool(*a || *b),
                (Expr::Str(a), Expr::Str(b), BinOp::Add) => Expr::Str(format!("{a}{b}")),
                (Expr::Int(a), Expr::Int(b), BinOp::Add) => Expr::Int(a + b),
                (Expr::Int(a), Expr::Int(b), BinOp::Sub) => Expr::Int(a - b),
                (Expr::Int(a), Expr::Int(b), BinOp::Mul) => Expr::Int(a * b),
                (Expr::Int(_), Expr::Int(0), BinOp::Div) => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                (Expr::Int(a), Expr::Int(b), BinOp::Div) => Expr::Int(a / b),
                (Expr::Int(_), Expr::Int(0), BinOp::Mod) => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                (Expr::Int(a), Expr::Int(b), BinOp::Mod) => Expr::Int(a % b),
                (Expr::Int(a), Expr::Int(b), BinOp::Eq) => Expr::Bool(a == b),
                (Expr::Int(a), Expr::Int(b), BinOp::Ne) => Expr::Bool(a != b),
                (Expr::Int(a), Expr::Int(b), BinOp::Lt) => Expr::Bool(a < b),
                (Expr::Int(a), Expr::Int(b), BinOp::Le) => Expr::Bool(a <= b),
                (Expr::Int(a), Expr::Int(b), BinOp::Gt) => Expr::Bool(a > b),
                (Expr::Int(a), Expr::Int(b), BinOp::Ge) => Expr::Bool(a >= b),
                (Expr::Bool(a), Expr::Bool(b), BinOp::Eq) => Expr::Bool(a == b),
                (Expr::Bool(a), Expr::Bool(b), BinOp::Ne) => Expr::Bool(a != b),
                (Expr::Str(a), Expr::Str(b), BinOp::Eq) => Expr::Bool(a == b),
                (Expr::Str(a), Expr::Str(b), BinOp::Ne) => Expr::Bool(a != b),
                _ => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            }
        }
        Expr::Call { name, args } => {
            let args = args
                .into_iter()
                .map(|a| fold_expr(a, ctx, current_fn, inline_depth))
                .collect::<Vec<_>>();

            if let Some(folded) = fold_compile_time_builtin(&name, &args) {
                return folded;
            }

            if inline_depth < ctx.max_inline_depth && name != current_fn {
                if let Some(template) = ctx.inlinable.get(&name) {
                    if template.params.len() == args.len() {
                        let mut bindings = HashMap::new();
                        for (param, arg) in template.params.iter().zip(args.iter()) {
                            bindings.insert(param.clone(), arg.clone());
                        }
                        let expanded = substitute_expr(&template.body, &bindings);
                        return fold_expr(expanded, ctx, current_fn, inline_depth + 1);
                    }
                }
            }

            Expr::Call { name, args }
        }
        other => other,
    }
}

fn fold_compile_time_builtin(name: &str, args: &[Expr]) -> Option<Expr> {
    match (name, args) {
        ("ct_hash", [Expr::Str(v)]) => Some(Expr::Int(builtins::ct_hash_str(v))),
        ("ct_xor", [Expr::Str(v), Expr::Int(k)]) => Some(Expr::Str(builtins::ct_xor_hex(v, *k))),
        ("xor_decode", [Expr::Str(v), Expr::Int(k)]) => {
            builtins::xor_decode_hex(v, *k).ok().map(Expr::Str)
        }
        _ => None,
    }
}

fn substitute_expr(expr: &Expr, bindings: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Int(v) => Expr::Int(*v),
        Expr::Bool(v) => Expr::Bool(*v),
        Expr::Str(v) => Expr::Str(v.clone()),
        Expr::Var(name) => bindings
            .get(name)
            .cloned()
            .unwrap_or_else(|| Expr::Var(name.clone())),
        Expr::Move(name) => Expr::Move(name.clone()),
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(substitute_expr(expr, bindings)),
        },
        Expr::Binary { op, left, right } => Expr::Binary {
            op: *op,
            left: Box::new(substitute_expr(left, bindings)),
            right: Box::new(substitute_expr(right, bindings)),
        },
        Expr::Call { name, args } => Expr::Call {
            name: name.clone(),
            args: args.iter().map(|a| substitute_expr(a, bindings)).collect(),
        },
    }
}

fn expr_uses_only_params(expr: &Expr, params: &HashSet<String>) -> bool {
    match expr {
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) => true,
        Expr::Var(name) => params.contains(name),
        Expr::Move(_) => false,
        Expr::Unary { expr, .. } => expr_uses_only_params(expr, params),
        Expr::Binary { left, right, .. } => {
            expr_uses_only_params(left, params) && expr_uses_only_params(right, params)
        }
        Expr::Call { args, .. } => args.iter().all(|a| expr_uses_only_params(a, params)),
    }
}
