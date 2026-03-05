use std::io::{self, Write};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::builtins;
use crate::memory;
use crate::runtime_args;

use super::Value;

pub(super) fn eval_builtin(name: &str, args: &[Value]) -> Result<Option<Value>, String> {
    let v = match name {
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
            Value::I64(0)
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
            Value::I64(0)
        }
        "input" => {
            if args.len() > 1 {
                return Err("builtin 'input' expects 0 or 1 argument".to_string());
            }
            if let Some(prompt) = args.first() {
                print!("{}", prompt.as_str()?);
                io::stdout().flush().map_err(|e| e.to_string())?;
            }
            let mut line = String::new();
            io::stdin()
                .read_line(&mut line)
                .map_err(|e| format!("input failed: {e}"))?;
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Value::Str(line)
        }
        "argc" => {
            if !args.is_empty() {
                return Err("builtin 'argc' expects 0 arguments".to_string());
            }
            Value::I64(runtime_args::argc())
        }
        "argv" => {
            if args.len() != 1 {
                return Err("builtin 'argv' expects 1 argument".to_string());
            }
            Value::Str(runtime_args::argv(args[0].as_i64()?))
        }
        "sleep_ms" => {
            if args.len() != 1 {
                return Err("builtin 'sleep_ms' expects 1 argument".to_string());
            }
            let ms = args[0].as_i64()?;
            if ms > 0 {
                thread::sleep(Duration::from_millis(ms as u64));
            }
            Value::I64(0)
        }
        "len" => {
            if args.len() != 1 {
                return Err("builtin 'len' expects 1 argument".to_string());
            }
            Value::I64(args[0].as_str()?.chars().count() as i64)
        }
        "clock_ms" => {
            if !args.is_empty() {
                return Err("builtin 'clock_ms' expects 0 arguments".to_string());
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?;
            Value::I64(now.as_millis() as i64)
        }
        "assert" => {
            if args.len() != 1 {
                return Err("builtin 'assert' expects 1 argument".to_string());
            }
            if args[0].as_bool()? {
                Value::I64(0)
            } else {
                return Err("assertion failed".to_string());
            }
        }
        "panic" => {
            if args.len() != 1 {
                return Err("builtin 'panic' expects 1 argument".to_string());
            }
            return Err(format!("panic: {}", args[0].as_text()));
        }
        "max" => {
            if args.len() != 2 {
                return Err("builtin 'max' expects 2 arguments".to_string());
            }
            Value::I64(args[0].as_i64()?.max(args[1].as_i64()?))
        }
        "min" => {
            if args.len() != 2 {
                return Err("builtin 'min' expects 2 arguments".to_string());
            }
            Value::I64(args[0].as_i64()?.min(args[1].as_i64()?))
        }
        "abs" => {
            if args.len() != 1 {
                return Err("builtin 'abs' expects 1 argument".to_string());
            }
            Value::I64(args[0].as_i64()?.abs())
        }
        "pow" => {
            if args.len() != 2 {
                return Err("builtin 'pow' expects 2 arguments".to_string());
            }
            let base = args[0].as_i64()?;
            let exp = args[1].as_i64()?;
            if exp < 0 {
                return Err("builtin 'pow' expects a non-negative exponent".to_string());
            }
            Value::I64(base.pow(exp as u32))
        }
        "clamp" => {
            if args.len() != 3 {
                return Err("builtin 'clamp' expects 3 arguments".to_string());
            }
            let v = args[0].as_i64()?;
            let lo = args[1].as_i64()?;
            let hi = args[2].as_i64()?;
            Value::I64(v.clamp(lo, hi))
        }
        "str" => {
            if args.len() != 1 {
                return Err("builtin 'str' expects 1 argument".to_string());
            }
            Value::Str(args[0].as_text())
        }
        "int" => {
            if args.len() != 1 {
                return Err("builtin 'int' expects 1 argument".to_string());
            }
            let out = match &args[0] {
                Value::I64(v) => *v,
                Value::Bool(v) => {
                    if *v {
                        1
                    } else {
                        0
                    }
                }
                Value::Str(v) => v
                    .parse::<i64>()
                    .map_err(|_| format!("cannot parse i64 from '{v}'"))?,
                Value::Void => return Err("cannot convert void to int".to_string()),
            };
            Value::I64(out)
        }
        "bool" => {
            if args.len() != 1 {
                return Err("builtin 'bool' expects 1 argument".to_string());
            }
            let out = match &args[0] {
                Value::I64(v) => *v != 0,
                Value::Bool(v) => *v,
                Value::Str(v) => !v.is_empty(),
                Value::Void => false,
            };
            Value::Bool(out)
        }
        "ord" => {
            if args.len() != 1 {
                return Err("builtin 'ord' expects 1 argument".to_string());
            }
            let s = args[0].as_str()?;
            let code = s.chars().next().map(|c| c as i64).unwrap_or(0);
            Value::I64(code)
        }
        "chr" => {
            if args.len() != 1 {
                return Err("builtin 'chr' expects 1 argument".to_string());
            }
            let v = args[0].as_i64()?;
            let out = u32::try_from(v)
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default();
            Value::Str(out)
        }
        "contains" => {
            if args.len() != 2 {
                return Err("builtin 'contains' expects 2 arguments".to_string());
            }
            Value::Bool(args[0].as_str()?.contains(args[1].as_str()?))
        }
        "find" => {
            if args.len() != 2 {
                return Err("builtin 'find' expects 2 arguments".to_string());
            }
            let hay = args[0].as_str()?;
            let needle = args[1].as_str()?;
            Value::I64(hay.find(needle).map(|i| i as i64).unwrap_or(-1))
        }
        "starts_with" => {
            if args.len() != 2 {
                return Err("builtin 'starts_with' expects 2 arguments".to_string());
            }
            Value::Bool(args[0].as_str()?.starts_with(args[1].as_str()?))
        }
        "ends_with" => {
            if args.len() != 2 {
                return Err("builtin 'ends_with' expects 2 arguments".to_string());
            }
            Value::Bool(args[0].as_str()?.ends_with(args[1].as_str()?))
        }
        "replace" => {
            if args.len() != 3 {
                return Err("builtin 'replace' expects 3 arguments".to_string());
            }
            Value::Str(
                args[0]
                    .as_str()?
                    .replace(args[1].as_str()?, args[2].as_str()?),
            )
        }
        "split" => {
            if args.len() != 3 {
                return Err("builtin 'split' expects 3 arguments".to_string());
            }
            let s = args[0].as_str()?;
            let sep = args[1].as_str()?;
            let idx = args[2].as_i64()?;
            if idx < 0 {
                Value::Str(String::new())
            } else {
                Value::Str(
                    s.split(sep)
                        .nth(idx as usize)
                        .unwrap_or_default()
                        .to_string(),
                )
            }
        }
        "join" => {
            if args.len() != 3 {
                return Err("builtin 'join' expects 3 arguments".to_string());
            }
            Value::Str(format!(
                "{}{}{}",
                args[0].as_str()?,
                args[2].as_str()?,
                args[1].as_str()?
            ))
        }
        "trim" => {
            if args.len() != 1 {
                return Err("builtin 'trim' expects 1 argument".to_string());
            }
            Value::Str(args[0].as_str()?.trim().to_string())
        }
        "upper" => {
            if args.len() != 1 {
                return Err("builtin 'upper' expects 1 argument".to_string());
            }
            Value::Str(args[0].as_str()?.to_ascii_uppercase())
        }
        "lower" => {
            if args.len() != 1 {
                return Err("builtin 'lower' expects 1 argument".to_string());
            }
            Value::Str(args[0].as_str()?.to_ascii_lowercase())
        }
        "exit" => {
            if args.len() != 1 {
                return Err("builtin 'exit' expects 1 argument".to_string());
            }
            Value::I64(args[0].as_i64()?)
        }
        "ptr_can_read" | "ptr_can_write" => {
            if args.len() != 2 {
                return Err(format!("builtin '{name}' expects 2 arguments"));
            }
            let addr = args[0].as_i64()?;
            let len = args[1].as_i64()?;
            let ok = if name == "ptr_can_read" {
                memory::can_read(addr, len)
            } else {
                memory::can_write(addr, len)
            };
            Value::Bool(ok)
        }
        "ptr_read8" => {
            if args.len() != 1 {
                return Err("builtin 'ptr_read8' expects 1 argument".to_string());
            }
            Value::I64(memory::read8(args[0].as_i64()?).unwrap_or(0))
        }
        "ptr_write8" => {
            if args.len() != 2 {
                return Err("builtin 'ptr_write8' expects 2 arguments".to_string());
            }
            let ok = memory::write8(args[0].as_i64()?, args[1].as_i64()?);
            Value::I64(if ok { 0 } else { -1 })
        }
        "ptr_read64" => {
            if args.len() != 1 {
                return Err("builtin 'ptr_read64' expects 1 argument".to_string());
            }
            Value::I64(memory::read64(args[0].as_i64()?).unwrap_or(0))
        }
        "ptr_write64" => {
            if args.len() != 2 {
                return Err("builtin 'ptr_write64' expects 2 arguments".to_string());
            }
            let ok = memory::write64(args[0].as_i64()?, args[1].as_i64()?);
            Value::I64(if ok { 0 } else { -1 })
        }
        "ct_hash" => {
            if args.len() != 1 {
                return Err("builtin 'ct_hash' expects 1 argument".to_string());
            }
            Value::I64(builtins::ct_hash_str(args[0].as_str()?))
        }
        "ct_xor" => {
            if args.len() != 2 {
                return Err("builtin 'ct_xor' expects 2 arguments".to_string());
            }
            Value::Str(builtins::ct_xor_hex(args[0].as_str()?, args[1].as_i64()?))
        }
        "xor_decode" => {
            if args.len() != 2 {
                return Err("builtin 'xor_decode' expects 2 arguments".to_string());
            }
            Value::Str(builtins::xor_decode_hex(
                args[0].as_str()?,
                args[1].as_i64()?,
            )?)
        }
        _ => return Ok(None),
    };
    Ok(Some(v))
}
