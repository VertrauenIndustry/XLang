use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::{BinOp, Expr, IsPattern, Program, Stmt, Type, UnaryOp};
use crate::builtins;
use crate::diag::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CtValue {
    I64(i64),
    Bool(bool),
    Str(String),
}

impl CtValue {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(*v),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(v) => Some(v.as_str()),
            _ => None,
        }
    }

    fn as_text(&self) -> String {
        match self {
            Self::I64(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Str(v) => v.clone(),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Self::I64(v) => *v != 0,
            Self::Bool(v) => *v,
            Self::Str(v) => !v.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtFlow {
    None,
    Break,
    Continue,
}

#[derive(Debug, Default)]
struct LowerState {
    const_env: HashMap<String, CtValue>,
    declared: HashSet<String>,
}

pub fn lower_comptime(program: &Program) -> Result<Program, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut functions = Vec::with_capacity(program.functions.len());

    for f in &program.functions {
        let mut state = LowerState::default();
        for p in &f.params {
            state.declared.insert(p.name.clone());
        }
        let body = lower_block(&f.body, &mut state, &mut errors);
        let mut next = f.clone();
        next.body = body;
        functions.push(next);
    }

    if errors.is_empty() {
        Ok(Program {
            functions,
            externs: program.externs.clone(),
            imports: program.imports.clone(),
        })
    } else {
        Err(errors)
    }
}

fn lower_block(block: &[Stmt], state: &mut LowerState, errors: &mut Vec<Diagnostic>) -> Vec<Stmt> {
    let mut out = Vec::new();

    for stmt in block {
        match stmt {
            Stmt::Comptime { body, line } => {
                let mut touched = Vec::new();
                let mut touched_set = HashSet::new();
                let mut budget = 100_000usize;
                let flow = exec_comptime_block(
                    body,
                    &mut state.const_env,
                    &mut touched,
                    &mut touched_set,
                    &mut budget,
                    errors,
                );
                if flow == CtFlow::Break || flow == CtFlow::Continue {
                    errors.push(Diagnostic::new(
                        *line,
                        "`break`/`continue` cannot escape comptime block",
                    ));
                }

                for name in touched {
                    let Some(val) = state.const_env.get(&name) else {
                        continue;
                    };
                    let expr = value_to_expr(val);
                    if state.declared.contains(&name) {
                        out.push(Stmt::Assign {
                            name,
                            expr,
                            line: *line,
                        });
                    } else {
                        state.declared.insert(name.clone());
                        out.push(Stmt::Let {
                            name,
                            ty: Type::Infer,
                            expr,
                            line: *line,
                        });
                    }
                }
            }
            Stmt::Let {
                name,
                ty,
                expr,
                line,
            } => {
                let lowered = fold_simple_const(expr.clone(), &state.const_env)
                    .unwrap_or_else(|| expr.clone());
                if let Some(v) = try_eval_expr(&lowered, &state.const_env) {
                    state.const_env.insert(name.clone(), v);
                } else {
                    state.const_env.remove(name);
                }
                state.declared.insert(name.clone());
                out.push(Stmt::Let {
                    name: name.clone(),
                    ty: ty.clone(),
                    expr: lowered,
                    line: *line,
                });
            }
            Stmt::Assign { name, expr, line } => {
                let lowered = fold_simple_const(expr.clone(), &state.const_env)
                    .unwrap_or_else(|| expr.clone());
                if let Some(v) = try_eval_expr(&lowered, &state.const_env) {
                    state.const_env.insert(name.clone(), v);
                } else {
                    state.const_env.remove(name);
                }
                state.declared.insert(name.clone());
                out.push(Stmt::Assign {
                    name: name.clone(),
                    expr: lowered,
                    line: *line,
                });
            }
            Stmt::Expr { expr, line } => {
                let lowered = fold_simple_const(expr.clone(), &state.const_env)
                    .unwrap_or_else(|| expr.clone());
                out.push(Stmt::Expr {
                    expr: lowered,
                    line: *line,
                });
            }
            Stmt::Return { expr, line } => {
                let lowered = fold_simple_const(expr.clone(), &state.const_env)
                    .unwrap_or_else(|| expr.clone());
                out.push(Stmt::Return {
                    expr: lowered,
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                line,
            } => {
                let mut then_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                let lowered_then = lower_block(then_body, &mut then_state, errors);
                let mut lowered_elif = Vec::with_capacity(elif_arms.len());
                for arm in elif_arms {
                    let mut arm_state = LowerState {
                        const_env: state.const_env.clone(),
                        declared: state.declared.clone(),
                    };
                    lowered_elif.push(crate::ast::ElifArm {
                        condition: fold_simple_const(arm.condition.clone(), &state.const_env)
                            .unwrap_or_else(|| arm.condition.clone()),
                        body: lower_block(&arm.body, &mut arm_state, errors),
                        line: arm.line,
                    });
                }
                let mut else_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                let lowered_else = lower_block(else_body, &mut else_state, errors);
                out.push(Stmt::If {
                    condition: fold_simple_const(condition.clone(), &state.const_env)
                        .unwrap_or_else(|| condition.clone()),
                    then_body: lowered_then,
                    elif_arms: lowered_elif,
                    else_body: lowered_else,
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                line,
            } => {
                let mut lowered_arms = Vec::with_capacity(arms.len());
                for arm in arms {
                    let mut arm_state = LowerState {
                        const_env: state.const_env.clone(),
                        declared: state.declared.clone(),
                    };
                    lowered_arms.push(crate::ast::IfIsArm {
                        patterns: arm
                            .patterns
                            .iter()
                            .map(|p| fold_pattern_const(p.clone(), &state.const_env))
                            .collect(),
                        body: lower_block(&arm.body, &mut arm_state, errors),
                        line: arm.line,
                    });
                }
                let mut else_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                let lowered_else = lower_block(else_body, &mut else_state, errors);
                out.push(Stmt::IfIs {
                    value: fold_simple_const(value.clone(), &state.const_env)
                        .unwrap_or_else(|| value.clone()),
                    arms: lowered_arms,
                    else_body: lowered_else,
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::While {
                condition,
                body,
                line,
            } => {
                let mut loop_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                out.push(Stmt::While {
                    condition: fold_simple_const(condition.clone(), &state.const_env)
                        .unwrap_or_else(|| condition.clone()),
                    body: lower_block(body, &mut loop_state, errors),
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::ThreadWhile {
                condition,
                body,
                count,
                wait,
                line,
            } => {
                let mut loop_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                out.push(Stmt::ThreadWhile {
                    condition: fold_simple_const(condition.clone(), &state.const_env)
                        .unwrap_or_else(|| condition.clone()),
                    body: lower_block(body, &mut loop_state, errors),
                    count: fold_simple_const(count.clone(), &state.const_env)
                        .unwrap_or_else(|| count.clone()),
                    wait: *wait,
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                line,
            } => {
                let mut loop_state = LowerState {
                    const_env: state.const_env.clone(),
                    declared: state.declared.clone(),
                };
                loop_state.declared.insert(var.clone());
                loop_state.const_env.remove(var);
                out.push(Stmt::ForRange {
                    var: var.clone(),
                    start: fold_simple_const(start.clone(), &state.const_env)
                        .unwrap_or_else(|| start.clone()),
                    end: fold_simple_const(end.clone(), &state.const_env)
                        .unwrap_or_else(|| end.clone()),
                    step: step.as_ref().map(|s| {
                        fold_simple_const(s.clone(), &state.const_env).unwrap_or_else(|| s.clone())
                    }),
                    body: lower_block(body, &mut loop_state, errors),
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::ThreadCall {
                call,
                count,
                wait,
                line,
            } => {
                out.push(Stmt::ThreadCall {
                    call: fold_simple_const(call.clone(), &state.const_env)
                        .unwrap_or_else(|| call.clone()),
                    count: fold_simple_const(count.clone(), &state.const_env)
                        .unwrap_or_else(|| count.clone()),
                    wait: *wait,
                    line: *line,
                });
                state.const_env.clear();
            }
            Stmt::Pass { line } => out.push(Stmt::Pass { line: *line }),
            Stmt::Break { line } => out.push(Stmt::Break { line: *line }),
            Stmt::Continue { line } => out.push(Stmt::Continue { line: *line }),
        }
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn exec_comptime_block(
    block: &[Stmt],
    env: &mut HashMap<String, CtValue>,
    touched: &mut Vec<String>,
    touched_set: &mut HashSet<String>,
    budget: &mut usize,
    errors: &mut Vec<Diagnostic>,
) -> CtFlow {
    for stmt in block {
        let flow = match stmt {
            Stmt::Let {
                name, expr, line, ..
            }
            | Stmt::Assign { name, expr, line } => {
                match eval_expr_strict(expr, env) {
                    Ok(v) => {
                        env.insert(name.clone(), v);
                        if touched_set.insert(name.clone()) {
                            touched.push(name.clone());
                        }
                    }
                    Err(msg) => errors.push(Diagnostic::new(*line, msg)),
                }
                CtFlow::None
            }
            Stmt::Expr { expr, line } => {
                if let Err(msg) = eval_expr_strict(expr, env) {
                    errors.push(Diagnostic::new(*line, msg));
                }
                CtFlow::None
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                line,
            } => {
                let cond = match eval_expr_strict(condition, env) {
                    Ok(v) => v.truthy(),
                    Err(msg) => {
                        errors.push(Diagnostic::new(*line, msg));
                        false
                    }
                };
                if cond {
                    exec_comptime_block(then_body, env, touched, touched_set, budget, errors)
                } else {
                    let mut taken = false;
                    let mut flow = CtFlow::None;
                    for arm in elif_arms {
                        let arm_cond = match eval_expr_strict(&arm.condition, env) {
                            Ok(v) => v.truthy(),
                            Err(msg) => {
                                errors.push(Diagnostic::new(arm.line, msg));
                                false
                            }
                        };
                        if arm_cond {
                            taken = true;
                            flow = exec_comptime_block(
                                &arm.body,
                                env,
                                touched,
                                touched_set,
                                budget,
                                errors,
                            );
                            break;
                        }
                    }
                    if !taken {
                        exec_comptime_block(else_body, env, touched, touched_set, budget, errors)
                    } else {
                        flow
                    }
                }
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                line,
            } => {
                let switch = match eval_expr_strict(value, env) {
                    Ok(v) => v,
                    Err(msg) => {
                        errors.push(Diagnostic::new(*line, msg));
                        continue;
                    }
                };
                let mut matched = false;
                let mut flow = CtFlow::None;
                for arm in arms {
                    for pattern in &arm.patterns {
                        match matches_is_pattern(pattern, &switch, env) {
                            Ok(true) => {
                                matched = true;
                                flow = exec_comptime_block(
                                    &arm.body,
                                    env,
                                    touched,
                                    touched_set,
                                    budget,
                                    errors,
                                );
                                break;
                            }
                            Ok(false) => {}
                            Err(msg) => errors.push(Diagnostic::new(arm.line, msg)),
                        }
                    }
                    if matched {
                        break;
                    }
                }
                if matched {
                    flow
                } else {
                    exec_comptime_block(else_body, env, touched, touched_set, budget, errors)
                }
            }
            Stmt::While {
                condition,
                body,
                line,
            } => {
                loop {
                    if *budget == 0 {
                        errors.push(Diagnostic::new(
                            *line,
                            "comptime loop exceeded iteration budget",
                        ));
                        break;
                    }
                    *budget -= 1;
                    let cond = match eval_expr_strict(condition, env) {
                        Ok(v) => v.truthy(),
                        Err(msg) => {
                            errors.push(Diagnostic::new(*line, msg));
                            false
                        }
                    };
                    if !cond {
                        break;
                    }
                    match exec_comptime_block(body, env, touched, touched_set, budget, errors) {
                        CtFlow::None | CtFlow::Continue => {}
                        CtFlow::Break => break,
                    }
                }
                CtFlow::None
            }
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                line,
            } => {
                let start_v = match eval_expr_strict(start, env).and_then(to_i64) {
                    Ok(v) => v,
                    Err(msg) => {
                        errors.push(Diagnostic::new(*line, msg));
                        continue;
                    }
                };
                let end_v = match eval_expr_strict(end, env).and_then(to_i64) {
                    Ok(v) => v,
                    Err(msg) => {
                        errors.push(Diagnostic::new(*line, msg));
                        continue;
                    }
                };
                let step_v = if let Some(step_expr) = step {
                    match eval_expr_strict(step_expr, env).and_then(to_i64) {
                        Ok(v) => v,
                        Err(msg) => {
                            errors.push(Diagnostic::new(*line, msg));
                            continue;
                        }
                    }
                } else {
                    1
                };
                if step_v == 0 {
                    errors.push(Diagnostic::new(
                        *line,
                        "comptime for-range step cannot be 0",
                    ));
                    continue;
                }
                let mut i = start_v;
                let cond = |cur: i64| if step_v > 0 { cur < end_v } else { cur > end_v };
                while cond(i) {
                    if *budget == 0 {
                        errors.push(Diagnostic::new(
                            *line,
                            "comptime loop exceeded iteration budget",
                        ));
                        break;
                    }
                    *budget -= 1;
                    env.insert(var.clone(), CtValue::I64(i));
                    if touched_set.insert(var.clone()) {
                        touched.push(var.clone());
                    }
                    match exec_comptime_block(body, env, touched, touched_set, budget, errors) {
                        CtFlow::None | CtFlow::Continue => i += step_v,
                        CtFlow::Break => break,
                    }
                }
                CtFlow::None
            }
            Stmt::ThreadWhile { line, .. } | Stmt::ThreadCall { line, .. } => {
                errors.push(Diagnostic::new(
                    *line,
                    "thread() syntax is runtime-only and cannot run inside comptime block",
                ));
                CtFlow::None
            }
            Stmt::Break { .. } => CtFlow::Break,
            Stmt::Continue { .. } => CtFlow::Continue,
            Stmt::Pass { .. } => CtFlow::None,
            Stmt::Return { line, .. } => {
                errors.push(Diagnostic::new(
                    *line,
                    "`return` is not allowed inside comptime block",
                ));
                CtFlow::None
            }
            Stmt::Comptime { body, .. } => {
                exec_comptime_block(body, env, touched, touched_set, budget, errors)
            }
        };
        if flow != CtFlow::None {
            return flow;
        }
    }
    CtFlow::None
}

fn value_to_expr(v: &CtValue) -> Expr {
    match v {
        CtValue::I64(x) => Expr::Int(*x),
        CtValue::Bool(x) => Expr::Bool(*x),
        CtValue::Str(x) => Expr::Str(x.clone()),
    }
}

fn fold_pattern_const(pattern: IsPattern, env: &HashMap<String, CtValue>) -> IsPattern {
    match pattern {
        IsPattern::Value(expr) => {
            IsPattern::Value(fold_simple_const(expr.clone(), env).unwrap_or(expr))
        }
        IsPattern::Ne(expr) => IsPattern::Ne(fold_simple_const(expr.clone(), env).unwrap_or(expr)),
        IsPattern::Lt(expr) => IsPattern::Lt(fold_simple_const(expr.clone(), env).unwrap_or(expr)),
        IsPattern::Le(expr) => IsPattern::Le(fold_simple_const(expr.clone(), env).unwrap_or(expr)),
        IsPattern::Gt(expr) => IsPattern::Gt(fold_simple_const(expr.clone(), env).unwrap_or(expr)),
        IsPattern::Ge(expr) => IsPattern::Ge(fold_simple_const(expr.clone(), env).unwrap_or(expr)),
        IsPattern::StartsWith(expr) => {
            IsPattern::StartsWith(fold_simple_const(expr.clone(), env).unwrap_or(expr))
        }
        IsPattern::EndsWith(expr) => {
            IsPattern::EndsWith(fold_simple_const(expr.clone(), env).unwrap_or(expr))
        }
        IsPattern::Contains(expr) => {
            IsPattern::Contains(fold_simple_const(expr.clone(), env).unwrap_or(expr))
        }
        IsPattern::Range { start, end } => IsPattern::Range {
            start: fold_simple_const(start.clone(), env).unwrap_or(start),
            end: fold_simple_const(end.clone(), env).unwrap_or(end),
        },
    }
}

fn fold_simple_const(expr: Expr, env: &HashMap<String, CtValue>) -> Option<Expr> {
    try_eval_expr(&expr, env).map(|v| value_to_expr(&v))
}

fn try_eval_expr(expr: &Expr, env: &HashMap<String, CtValue>) -> Option<CtValue> {
    match expr {
        Expr::Int(v) => Some(CtValue::I64(*v)),
        Expr::Bool(v) => Some(CtValue::Bool(*v)),
        Expr::Str(v) => Some(CtValue::Str(v.clone())),
        Expr::Var(name) | Expr::Move(name) => env.get(name).cloned(),
        Expr::Unary { op, expr } => {
            let v = try_eval_expr(expr, env)?;
            match op {
                UnaryOp::Neg => Some(CtValue::I64(v.as_i64()? * -1)),
                UnaryOp::Not => Some(CtValue::Bool(!v.as_bool()?)),
            }
        }
        Expr::Binary { op, left, right } => {
            let l = try_eval_expr(left, env)?;
            let r = try_eval_expr(right, env)?;
            match op {
                BinOp::Add => match (&l, &r) {
                    (CtValue::Str(a), CtValue::Str(b)) => Some(CtValue::Str(format!("{a}{b}"))),
                    _ => Some(CtValue::I64(l.as_i64()? + r.as_i64()?)),
                },
                BinOp::Sub => Some(CtValue::I64(l.as_i64()? - r.as_i64()?)),
                BinOp::Mul => Some(CtValue::I64(l.as_i64()? * r.as_i64()?)),
                BinOp::Div => Some(CtValue::I64(l.as_i64()? / r.as_i64()?)),
                BinOp::Mod => Some(CtValue::I64(l.as_i64()? % r.as_i64()?)),
                BinOp::Eq => Some(CtValue::Bool(l == r)),
                BinOp::Ne => Some(CtValue::Bool(l != r)),
                BinOp::Lt => Some(CtValue::Bool(l.as_i64()? < r.as_i64()?)),
                BinOp::Le => Some(CtValue::Bool(l.as_i64()? <= r.as_i64()?)),
                BinOp::Gt => Some(CtValue::Bool(l.as_i64()? > r.as_i64()?)),
                BinOp::Ge => Some(CtValue::Bool(l.as_i64()? >= r.as_i64()?)),
                BinOp::And => Some(CtValue::Bool(l.as_bool()? && r.as_bool()?)),
                BinOp::Or => Some(CtValue::Bool(l.as_bool()? || r.as_bool()?)),
            }
        }
        Expr::Call { name, args } => {
            let args = args
                .iter()
                .map(|a| try_eval_expr(a, env))
                .collect::<Option<Vec<_>>>()?;
            eval_builtin_pure(name, &args).ok()
        }
    }
}

fn eval_expr_strict(expr: &Expr, env: &HashMap<String, CtValue>) -> Result<CtValue, String> {
    match expr {
        Expr::Int(v) => Ok(CtValue::I64(*v)),
        Expr::Bool(v) => Ok(CtValue::Bool(*v)),
        Expr::Str(v) => Ok(CtValue::Str(v.clone())),
        Expr::Var(name) | Expr::Move(name) => env
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown/non-constant variable '{name}' in comptime block")),
        Expr::Unary { op, expr } => {
            let v = eval_expr_strict(expr, env)?;
            match op {
                UnaryOp::Neg => Ok(CtValue::I64(to_i64(v)? * -1)),
                UnaryOp::Not => Ok(CtValue::Bool(!to_bool(v)?)),
            }
        }
        Expr::Binary { op, left, right } => {
            let l = eval_expr_strict(left, env)?;
            let r = eval_expr_strict(right, env)?;
            match op {
                BinOp::Add => match (&l, &r) {
                    (CtValue::Str(a), CtValue::Str(b)) => Ok(CtValue::Str(format!("{a}{b}"))),
                    _ => Ok(CtValue::I64(to_i64(l)? + to_i64(r)?)),
                },
                BinOp::Sub => Ok(CtValue::I64(to_i64(l)? - to_i64(r)?)),
                BinOp::Mul => Ok(CtValue::I64(to_i64(l)? * to_i64(r)?)),
                BinOp::Div => Ok(CtValue::I64(to_i64(l)? / to_i64(r)?)),
                BinOp::Mod => Ok(CtValue::I64(to_i64(l)? % to_i64(r)?)),
                BinOp::Eq => Ok(CtValue::Bool(l == r)),
                BinOp::Ne => Ok(CtValue::Bool(l != r)),
                BinOp::Lt => Ok(CtValue::Bool(to_i64(l)? < to_i64(r)?)),
                BinOp::Le => Ok(CtValue::Bool(to_i64(l)? <= to_i64(r)?)),
                BinOp::Gt => Ok(CtValue::Bool(to_i64(l)? > to_i64(r)?)),
                BinOp::Ge => Ok(CtValue::Bool(to_i64(l)? >= to_i64(r)?)),
                BinOp::And => Ok(CtValue::Bool(to_bool(l)? && to_bool(r)?)),
                BinOp::Or => Ok(CtValue::Bool(to_bool(l)? || to_bool(r)?)),
            }
        }
        Expr::Call { name, args } => {
            let args = args
                .iter()
                .map(|a| eval_expr_strict(a, env))
                .collect::<Result<Vec<_>, _>>()?;
            eval_builtin_strict(name, &args)
        }
    }
}

fn matches_is_pattern(
    pattern: &IsPattern,
    value: &CtValue,
    env: &HashMap<String, CtValue>,
) -> Result<bool, String> {
    match pattern {
        IsPattern::Value(expr) => Ok(*value == eval_expr_strict(expr, env)?),
        IsPattern::Ne(expr) => Ok(*value != eval_expr_strict(expr, env)?),
        IsPattern::Lt(expr) => Ok(to_i64(value.clone())? < to_i64(eval_expr_strict(expr, env)?)?),
        IsPattern::Le(expr) => Ok(to_i64(value.clone())? <= to_i64(eval_expr_strict(expr, env)?)?),
        IsPattern::Gt(expr) => Ok(to_i64(value.clone())? > to_i64(eval_expr_strict(expr, env)?)?),
        IsPattern::Ge(expr) => Ok(to_i64(value.clone())? >= to_i64(eval_expr_strict(expr, env)?)?),
        IsPattern::StartsWith(expr) => Ok({
            let v = to_str(value.clone())?;
            let pat = to_str(eval_expr_strict(expr, env)?)?;
            v.starts_with(&pat)
        }),
        IsPattern::EndsWith(expr) => Ok({
            let v = to_str(value.clone())?;
            let pat = to_str(eval_expr_strict(expr, env)?)?;
            v.ends_with(&pat)
        }),
        IsPattern::Contains(expr) => Ok({
            let v = to_str(value.clone())?;
            let pat = to_str(eval_expr_strict(expr, env)?)?;
            v.contains(&pat)
        }),
        IsPattern::Range { start, end } => {
            let v = to_i64(value.clone())?;
            let s = to_i64(eval_expr_strict(start, env)?)?;
            let e = to_i64(eval_expr_strict(end, env)?)?;
            Ok(v >= s && v < e)
        }
    }
}

fn eval_builtin_pure(name: &str, args: &[CtValue]) -> Result<CtValue, String> {
    match name {
        "max" if args.len() == 2 => Ok(CtValue::I64(
            to_i64(args[0].clone())?.max(to_i64(args[1].clone())?),
        )),
        "min" if args.len() == 2 => Ok(CtValue::I64(
            to_i64(args[0].clone())?.min(to_i64(args[1].clone())?),
        )),
        "abs" if args.len() == 1 => Ok(CtValue::I64(to_i64(args[0].clone())?.abs())),
        "pow" if args.len() == 2 => {
            let base = to_i64(args[0].clone())?;
            let exp = to_i64(args[1].clone())?;
            if exp < 0 {
                return Err("builtin 'pow' expects a non-negative exponent".to_string());
            }
            Ok(CtValue::I64(base.pow(exp as u32)))
        }
        "clamp" if args.len() == 3 => Ok(CtValue::I64(
            to_i64(args[0].clone())?.clamp(to_i64(args[1].clone())?, to_i64(args[2].clone())?),
        )),
        "str" if args.len() == 1 => Ok(CtValue::Str(args[0].as_text())),
        "int" if args.len() == 1 => match &args[0] {
            CtValue::I64(v) => Ok(CtValue::I64(*v)),
            CtValue::Bool(v) => Ok(CtValue::I64(if *v { 1 } else { 0 })),
            CtValue::Str(v) => v
                .parse::<i64>()
                .map(CtValue::I64)
                .map_err(|_| format!("cannot parse i64 from '{v}'")),
        },
        "bool" if args.len() == 1 => match &args[0] {
            CtValue::I64(v) => Ok(CtValue::Bool(*v != 0)),
            CtValue::Bool(v) => Ok(CtValue::Bool(*v)),
            CtValue::Str(v) => Ok(CtValue::Bool(!v.is_empty())),
        },
        "len" if args.len() == 1 => {
            Ok(CtValue::I64(to_str(args[0].clone())?.chars().count() as i64))
        }
        "ord" if args.len() == 1 => {
            let s = to_str(args[0].clone())?;
            Ok(CtValue::I64(
                s.chars().next().map(|c| c as i64).unwrap_or(0),
            ))
        }
        "chr" if args.len() == 1 => {
            let v = to_i64(args[0].clone())?;
            let out = u32::try_from(v)
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default();
            Ok(CtValue::Str(out))
        }
        "contains" if args.len() == 2 => {
            let hay = to_str(args[0].clone())?;
            let needle = to_str(args[1].clone())?;
            Ok(CtValue::Bool(hay.contains(&needle)))
        }
        "find" if args.len() == 2 => {
            let hay = to_str(args[0].clone())?;
            let needle = to_str(args[1].clone())?;
            Ok(CtValue::I64(hay.find(&needle).map(|i| i as i64).unwrap_or(-1)))
        }
        "starts_with" if args.len() == 2 => {
            let s = to_str(args[0].clone())?;
            let prefix = to_str(args[1].clone())?;
            Ok(CtValue::Bool(s.starts_with(&prefix)))
        }
        "ends_with" if args.len() == 2 => {
            let s = to_str(args[0].clone())?;
            let suffix = to_str(args[1].clone())?;
            Ok(CtValue::Bool(s.ends_with(&suffix)))
        }
        "replace" if args.len() == 3 => {
            let s = to_str(args[0].clone())?;
            let from = to_str(args[1].clone())?;
            let to = to_str(args[2].clone())?;
            Ok(CtValue::Str(s.replace(&from, &to)))
        }
        "split" if args.len() == 3 => {
            let s = to_str(args[0].clone())?;
            let sep = to_str(args[1].clone())?;
            let idx = to_i64(args[2].clone())?;
            if idx < 0 {
                Ok(CtValue::Str(String::new()))
            } else {
                Ok(CtValue::Str(
                    s.split(&sep).nth(idx as usize).unwrap_or_default().to_string(),
                ))
            }
        }
        "join" if args.len() == 3 => {
            let a = to_str(args[0].clone())?;
            let b = to_str(args[1].clone())?;
            let sep = to_str(args[2].clone())?;
            Ok(CtValue::Str(format!("{a}{sep}{b}")))
        }
        "trim" if args.len() == 1 => Ok(CtValue::Str(to_str(args[0].clone())?.trim().to_string())),
        "upper" if args.len() == 1 => {
            Ok(CtValue::Str(to_str(args[0].clone())?.to_ascii_uppercase()))
        }
        "lower" if args.len() == 1 => {
            Ok(CtValue::Str(to_str(args[0].clone())?.to_ascii_lowercase()))
        }
        "ct_hash" if args.len() == 1 => {
            let s = to_str(args[0].clone())?;
            Ok(CtValue::I64(builtins::ct_hash_str(&s)))
        }
        "ct_xor" if args.len() == 2 => {
            let s = to_str(args[0].clone())?;
            Ok(CtValue::Str(builtins::ct_xor_hex(
                &s,
                to_i64(args[1].clone())?,
            )))
        }
        "xor_decode" if args.len() == 2 => {
            let s = to_str(args[0].clone())?;
            Ok(CtValue::Str(builtins::xor_decode_hex(
                &s,
                to_i64(args[1].clone())?,
            )?))
        }
        _ => Err(format!("unsupported compile-time call '{name}'")),
    }
}

fn eval_builtin_strict(name: &str, args: &[CtValue]) -> Result<CtValue, String> {
    match name {
        "write" => {
            if args.is_empty() {
                return Err("builtin 'write' expects at least 1 argument".to_string());
            }
            for (idx, arg) in args.iter().enumerate() {
                if idx > 0 {
                    print!(" ");
                }
                print!("{}", arg.as_text());
            }
            io::stdout().flush().map_err(|e| e.to_string())?;
            Ok(CtValue::I64(0))
        }
        "print" | "println" => {
            if args.is_empty() {
                println!();
            } else {
                for (idx, arg) in args.iter().enumerate() {
                    if idx > 0 {
                        print!(" ");
                    }
                    print!("{}", arg.as_text());
                }
                println!();
            }
            Ok(CtValue::I64(0))
        }
        "clock_ms" => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?;
            Ok(CtValue::I64(now.as_millis() as i64))
        }
        "assert" => {
            if args.len() != 1 {
                return Err("builtin 'assert' expects 1 argument".to_string());
            }
            if to_bool(args[0].clone())? {
                Ok(CtValue::I64(0))
            } else {
                Err("assertion failed".to_string())
            }
        }
        "panic" => {
            if args.len() != 1 {
                return Err("builtin 'panic' expects 1 argument".to_string());
            }
            Err(format!("panic: {}", args[0].as_text()))
        }
        "exit" => {
            if args.len() != 1 {
                return Err("builtin 'exit' expects 1 argument".to_string());
            }
            Ok(CtValue::I64(to_i64(args[0].clone())?))
        }
        "input" | "argc" | "argv" | "sleep_ms" => Err(format!(
            "builtin '{name}' is runtime-only and cannot be used in comptime"
        )),
        _ => eval_builtin_pure(name, args),
    }
}

fn to_i64(v: CtValue) -> Result<i64, String> {
    v.as_i64().ok_or_else(|| "expected i64 value".to_string())
}

fn to_bool(v: CtValue) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| "expected bool value".to_string())
}

fn to_str(v: CtValue) -> Result<String, String> {
    v.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "expected str value".to_string())
}
