use std::collections::HashMap;

use crate::ast::{BinOp, Expr, ExternFunction, IsPattern, Program, Stmt, UnaryOp};

mod builtins_runtime;
mod ffi;

use self::builtins_runtime::eval_builtin;
use self::ffi::{eval_extern_call, FfiRuntime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub exit_code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    I64(i64),
    Bool(bool),
    Str(String),
    Void,
}

impl Value {
    fn as_i64(&self) -> Result<i64, String> {
        match self {
            Value::I64(v) => Ok(*v),
            _ => Err("expected i64 value".to_string()),
        }
    }

    fn as_bool(&self) -> Result<bool, String> {
        match self {
            Value::Bool(v) => Ok(*v),
            _ => Err("expected bool value".to_string()),
        }
    }

    fn as_str(&self) -> Result<&str, String> {
        match self {
            Value::Str(v) => Ok(v.as_str()),
            _ => Err("expected str value".to_string()),
        }
    }

    fn as_text(&self) -> String {
        match self {
            Value::I64(v) => v.to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Str(v) => v.clone(),
            Value::Void => "<void>".to_string(),
        }
    }
}

pub fn run(program: &Program) -> Result<RunResult, String> {
    let mut functions = HashMap::new();
    for f in &program.functions {
        functions.insert(f.name.clone(), f.clone());
    }
    let mut externs = HashMap::new();
    for ext in &program.externs {
        externs.insert(ext.name.clone(), ext.clone());
    }
    let mut ffi = FfiRuntime::new(externs.clone());

    let value = eval_fn("main", &[], &functions, &externs, &mut ffi)?;
    Ok(RunResult {
        exit_code: value.as_i64()?,
    })
}

fn eval_fn(
    name: &str,
    args: &[Value],
    functions: &HashMap<String, crate::ast::Function>,
    externs: &HashMap<String, ExternFunction>,
    ffi: &mut FfiRuntime,
) -> Result<Value, String> {
    let Some(f) = functions.get(name) else {
        return Err(format!("function '{name}' not found"));
    };
    if f.params.len() != args.len() {
        return Err(format!(
            "function '{name}' expected {} args, got {}",
            f.params.len(),
            args.len()
        ));
    }

    let mut env: HashMap<String, Value> = HashMap::new();
    for (idx, p) in f.params.iter().enumerate() {
        env.insert(p.name.clone(), args[idx].clone());
    }

    match eval_block(&f.body, &mut env, functions, externs, ffi, false)? {
        ControlFlow::Return(v) => Ok(v),
        ControlFlow::None => Ok(Value::Void),
        ControlFlow::Break => Err("`break` used outside loop".to_string()),
        ControlFlow::Continue => Err("`continue` used outside loop".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlFlow {
    None,
    Return(Value),
    Break,
    Continue,
}

fn eval_block(
    block: &[Stmt],
    env: &mut HashMap<String, Value>,
    functions: &HashMap<String, crate::ast::Function>,
    externs: &HashMap<String, ExternFunction>,
    ffi: &mut FfiRuntime,
    in_loop: bool,
) -> Result<ControlFlow, String> {
    for stmt in block {
        match stmt {
            Stmt::Let { name, expr, .. } => {
                let val = eval_expr(expr, env, functions, externs, ffi)?;
                env.insert(name.clone(), val);
            }
            Stmt::Assign { name, expr, .. } => {
                let val = eval_expr(expr, env, functions, externs, ffi)?;
                env.insert(name.clone(), val);
            }
            Stmt::Expr { expr, .. } => {
                let _ = eval_expr(expr, env, functions, externs, ffi)?;
            }
            Stmt::Return { expr, .. } => {
                let val = eval_expr(expr, env, functions, externs, ffi)?;
                return Ok(ControlFlow::Return(val));
            }
            Stmt::IfIs {
                value,
                arms,
                else_body,
                ..
            } => {
                let value_v = eval_expr(value, env, functions, externs, ffi)?;
                let mut matched = false;
                for arm in arms {
                    for pattern in &arm.patterns {
                        if matches_is_pattern(pattern, &value_v, env, functions, externs, ffi)? {
                            matched = true;
                            match eval_block(&arm.body, env, functions, externs, ffi, in_loop)? {
                                ControlFlow::None => {}
                                flow => return Ok(flow),
                            }
                            break;
                        }
                    }
                    if matched {
                        break;
                    }
                }
                if !matched && !else_body.is_empty() {
                    match eval_block(else_body, env, functions, externs, ffi, in_loop)? {
                        ControlFlow::None => {}
                        flow => return Ok(flow),
                    }
                }
            }
            Stmt::If {
                condition,
                then_body,
                elif_arms,
                else_body,
                ..
            } => {
                let condition_value = eval_expr(condition, env, functions, externs, ffi)?;
                if is_truthy(&condition_value) {
                    match eval_block(then_body, env, functions, externs, ffi, in_loop)? {
                        ControlFlow::None => {}
                        flow => return Ok(flow),
                    }
                } else {
                    let mut taken = false;
                    for arm in elif_arms {
                        let arm_cond = eval_expr(&arm.condition, env, functions, externs, ffi)?;
                        if is_truthy(&arm_cond) {
                            taken = true;
                            match eval_block(&arm.body, env, functions, externs, ffi, in_loop)? {
                                ControlFlow::None => {}
                                flow => return Ok(flow),
                            }
                            break;
                        }
                    }
                    if !taken && !else_body.is_empty() {
                        match eval_block(else_body, env, functions, externs, ffi, in_loop)? {
                            ControlFlow::None => {}
                            flow => return Ok(flow),
                        }
                    }
                }
            }
            Stmt::While {
                condition, body, ..
            } => loop {
                let cond = eval_expr(condition, env, functions, externs, ffi)?;
                if !is_truthy(&cond) {
                    break;
                }
                match eval_block(body, env, functions, externs, ffi, true)? {
                    ControlFlow::None | ControlFlow::Continue => continue,
                    ControlFlow::Break => break,
                    ControlFlow::Return(v) => return Ok(ControlFlow::Return(v)),
                }
            },
            Stmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                ..
            } => {
                let start_v = eval_expr(start, env, functions, externs, ffi)?.as_i64()?;
                let end_v = eval_expr(end, env, functions, externs, ffi)?.as_i64()?;
                let step_v = if let Some(step_expr) = step {
                    eval_expr(step_expr, env, functions, externs, ffi)?.as_i64()?
                } else {
                    1
                };
                if step_v == 0 {
                    return Err("for-range step cannot be 0".to_string());
                }
                let mut i = start_v;
                let cond = |cur: i64| if step_v > 0 { cur < end_v } else { cur > end_v };
                while cond(i) {
                    env.insert(var.clone(), Value::I64(i));
                    match eval_block(body, env, functions, externs, ffi, true)? {
                        ControlFlow::None | ControlFlow::Continue => {
                            i += step_v;
                            continue;
                        }
                        ControlFlow::Break => break,
                        ControlFlow::Return(v) => return Ok(ControlFlow::Return(v)),
                    }
                }
            }
            Stmt::Comptime { body, .. } => {
                match eval_block(body, env, functions, externs, ffi, in_loop)? {
                    ControlFlow::None => {}
                    flow => return Ok(flow),
                }
            }
            Stmt::ThreadCall {
                call,
                count,
                wait,
                line,
            } => {
                let n = eval_expr(count, env, functions, externs, ffi)?.as_i64()?;
                if n <= 0 {
                    return Err(format!("line {line}: thread count must be > 0"));
                }
                let Expr::Call { name, args } = call else {
                    return Err(format!(
                        "line {line}: thread() currently supports only function call statements"
                    ));
                };
                let mut call_args = Vec::with_capacity(args.len());
                for arg in args {
                    call_args.push(eval_expr(arg, env, functions, externs, ffi)?);
                }
                let mut handles = Vec::new();
                for _ in 0..n {
                    let fn_name = name.clone();
                    let arg_values = call_args.clone();
                    let functions_cloned = functions.clone();
                    let externs_cloned = externs.clone();
                    handles.push(std::thread::spawn(move || -> Result<(), String> {
                        let mut ffi = FfiRuntime::new(externs_cloned.clone());
                        let _ = eval_fn(
                            &fn_name,
                            &arg_values,
                            &functions_cloned,
                            &externs_cloned,
                            &mut ffi,
                        )?;
                        Ok(())
                    }));
                }
                if *wait {
                    for h in handles {
                        match h.join() {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => return Err(e),
                            Err(_) => return Err("thread panicked".to_string()),
                        }
                    }
                }
            }
            Stmt::ThreadWhile {
                condition,
                body,
                count,
                wait,
                line,
            } => {
                let n = eval_expr(count, env, functions, externs, ffi)?.as_i64()?;
                if n <= 0 {
                    return Err(format!("line {line}: thread count must be > 0"));
                }
                let mut handles = Vec::new();
                for _ in 0..n {
                    let cond = condition.clone();
                    let loop_body = body.clone();
                    let mut env_cloned = env.clone();
                    let functions_cloned = functions.clone();
                    let externs_cloned = externs.clone();
                    handles.push(std::thread::spawn(move || -> Result<(), String> {
                        let mut ffi = FfiRuntime::new(externs_cloned.clone());
                        let synthetic = Stmt::While {
                            condition: cond,
                            body: loop_body,
                            line: 0,
                        };
                        match eval_block(
                            &[synthetic],
                            &mut env_cloned,
                            &functions_cloned,
                            &externs_cloned,
                            &mut ffi,
                            false,
                        )? {
                            ControlFlow::None => Ok(()),
                            ControlFlow::Return(_) => Ok(()),
                            ControlFlow::Break | ControlFlow::Continue => Ok(()),
                        }
                    }));
                }
                if *wait {
                    for h in handles {
                        match h.join() {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => return Err(e),
                            Err(_) => return Err("thread panicked".to_string()),
                        }
                    }
                }
            }
            Stmt::Pass { .. } => {}
            Stmt::Break { .. } => {
                if in_loop {
                    return Ok(ControlFlow::Break);
                }
                return Err("`break` used outside loop".to_string());
            }
            Stmt::Continue { .. } => {
                if in_loop {
                    return Ok(ControlFlow::Continue);
                }
                return Err("`continue` used outside loop".to_string());
            }
        }
    }
    Ok(ControlFlow::None)
}

fn matches_is_pattern(
    pattern: &IsPattern,
    value: &Value,
    env: &mut HashMap<String, Value>,
    functions: &HashMap<String, crate::ast::Function>,
    externs: &HashMap<String, ExternFunction>,
    ffi: &mut FfiRuntime,
) -> Result<bool, String> {
    match pattern {
        IsPattern::Value(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(values_equal(value, &p))
        }
        IsPattern::Ne(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(!values_equal(value, &p))
        }
        IsPattern::Lt(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_i64()? < p.as_i64()?)
        }
        IsPattern::Le(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_i64()? <= p.as_i64()?)
        }
        IsPattern::Gt(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_i64()? > p.as_i64()?)
        }
        IsPattern::Ge(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_i64()? >= p.as_i64()?)
        }
        IsPattern::StartsWith(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_str()?.starts_with(p.as_str()?))
        }
        IsPattern::EndsWith(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_str()?.ends_with(p.as_str()?))
        }
        IsPattern::Contains(expr) => {
            let p = eval_expr(expr, env, functions, externs, ffi)?;
            Ok(value.as_str()?.contains(p.as_str()?))
        }
        IsPattern::Range { start, end } => {
            let s = eval_expr(start, env, functions, externs, ffi)?.as_i64()?;
            let e = eval_expr(end, env, functions, externs, ffi)?.as_i64()?;
            let v = value.as_i64()?;
            Ok(v >= s && v < e)
        }
    }
}

fn eval_expr(
    expr: &Expr,
    env: &mut HashMap<String, Value>,
    functions: &HashMap<String, crate::ast::Function>,
    externs: &HashMap<String, ExternFunction>,
    ffi: &mut FfiRuntime,
) -> Result<Value, String> {
    match expr {
        Expr::Int(v) => Ok(Value::I64(*v)),
        Expr::Bool(v) => Ok(Value::Bool(*v)),
        Expr::Str(v) => Ok(Value::Str(v.clone())),
        Expr::Var(name) => env
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown variable '{name}'")),
        Expr::Move(name) => env
            .remove(name)
            .ok_or_else(|| format!("cannot move unknown variable '{name}'")),
        Expr::Unary { op, expr } => {
            let v = eval_expr(expr, env, functions, externs, ffi)?;
            match op {
                UnaryOp::Neg => Ok(Value::I64(-v.as_i64()?)),
                UnaryOp::Not => Ok(Value::Bool(!v.as_bool()?)),
            }
        }
        Expr::Binary { op, left, right } => {
            let l = eval_expr(left, env, functions, externs, ffi)?;
            let r = eval_expr(right, env, functions, externs, ffi)?;
            match op {
                BinOp::Add => match (&l, &r) {
                    (Value::Str(ls), Value::Str(rs)) => Ok(Value::Str(format!("{ls}{rs}"))),
                    _ => Ok(Value::I64(l.as_i64()? + r.as_i64()?)),
                },
                BinOp::Sub => Ok(Value::I64(l.as_i64()? - r.as_i64()?)),
                BinOp::Mul => Ok(Value::I64(l.as_i64()? * r.as_i64()?)),
                BinOp::Div => Ok(Value::I64(l.as_i64()? / r.as_i64()?)),
                BinOp::Mod => Ok(Value::I64(l.as_i64()? % r.as_i64()?)),
                BinOp::Eq => Ok(Value::Bool(values_equal(&l, &r))),
                BinOp::Ne => Ok(Value::Bool(!values_equal(&l, &r))),
                BinOp::Lt => Ok(Value::Bool(l.as_i64()? < r.as_i64()?)),
                BinOp::Le => Ok(Value::Bool(l.as_i64()? <= r.as_i64()?)),
                BinOp::Gt => Ok(Value::Bool(l.as_i64()? > r.as_i64()?)),
                BinOp::Ge => Ok(Value::Bool(l.as_i64()? >= r.as_i64()?)),
                BinOp::And => Ok(Value::Bool(l.as_bool()? && r.as_bool()?)),
                BinOp::Or => Ok(Value::Bool(l.as_bool()? || r.as_bool()?)),
            }
        }
        Expr::Call { name, args } => {
            let mut arg_values = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(eval_expr(arg, env, functions, externs, ffi)?);
            }
            if let Some(v) = eval_builtin(name, &arg_values)? {
                return Ok(v);
            }
            if externs.contains_key(name) {
                return eval_extern_call(name, &arg_values, ffi);
            }
            eval_fn(name, &arg_values, functions, externs, ffi)
        }
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        _ => false,
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::I64(x) => *x != 0,
        Value::Bool(x) => *x,
        Value::Str(s) => !s.is_empty(),
        Value::Void => false,
    }
}
