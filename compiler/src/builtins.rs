use crate::ast::Type;

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "write"
            | "println"
            | "input"
            | "argc"
            | "argv"
            | "sleep_ms"
            | "len"
            | "clock_ms"
            | "assert"
            | "panic"
            | "max"
            | "min"
            | "abs"
            | "pow"
            | "clamp"
            | "str"
            | "int"
            | "bool"
            | "ord"
            | "chr"
            | "contains"
            | "find"
            | "starts_with"
            | "ends_with"
            | "split"
            | "join"
            | "replace"
            | "trim"
            | "upper"
            | "lower"
            | "exit"
            | "ptr_can_read"
            | "ptr_can_write"
            | "ptr_read8"
            | "ptr_write8"
            | "ptr_read64"
            | "ptr_write64"
            | "ct_hash"
            | "ct_xor"
            | "xor_decode"
    )
}

pub fn return_type(name: &str, arg_types: &[Type]) -> Option<Result<Type, String>> {
    let out = match name {
        "print" => {
            if arg_types
                .iter()
                .any(|t| !matches!(t, Type::I64 | Type::Bool | Type::Str))
            {
                Err("builtin 'print' only supports i64, bool, or str arguments".to_string())
            } else {
                Ok(Type::I64)
            }
        }
        "write" => {
            if arg_types.is_empty() {
                Err("builtin 'write' expects at least 1 argument".to_string())
            } else if arg_types
                .iter()
                .any(|t| !matches!(t, Type::I64 | Type::Bool | Type::Str))
            {
                Err("builtin 'write' only supports i64, bool, or str arguments".to_string())
            } else {
                Ok(Type::I64)
            }
        }
        "println" => {
            if arg_types
                .iter()
                .any(|t| !matches!(t, Type::I64 | Type::Bool | Type::Str))
            {
                Err("builtin 'println' only supports i64, bool, or str arguments".to_string())
            } else {
                Ok(Type::I64)
            }
        }
        "input" => {
            if arg_types.is_empty() {
                Ok(Type::Str)
            } else if arg_types == [Type::Str] {
                Ok(Type::Str)
            } else {
                Err("builtin 'input' expects () or (str)".to_string())
            }
        }
        "argc" => {
            if arg_types.is_empty() {
                Ok(Type::I64)
            } else {
                Err("builtin 'argc' expects 0 arguments".to_string())
            }
        }
        "argv" => {
            if arg_types == [Type::I64] {
                Ok(Type::Str)
            } else {
                Err("builtin 'argv' expects exactly (i64)".to_string())
            }
        }
        "sleep_ms" => {
            if arg_types == [Type::I64] {
                Ok(Type::I64)
            } else {
                Err("builtin 'sleep_ms' expects exactly (i64)".to_string())
            }
        }
        "len" => {
            if arg_types == [Type::Str] {
                Ok(Type::I64)
            } else {
                Err("builtin 'len' expects exactly (str)".to_string())
            }
        }
        "clock_ms" => {
            if arg_types.is_empty() {
                Ok(Type::I64)
            } else {
                Err("builtin 'clock_ms' expects 0 arguments".to_string())
            }
        }
        "assert" => {
            if arg_types == [Type::Bool] {
                Ok(Type::I64)
            } else {
                Err("builtin 'assert' expects exactly (bool)".to_string())
            }
        }
        "panic" => {
            if arg_types.len() != 1 {
                Err("builtin 'panic' expects 1 argument".to_string())
            } else if !matches!(arg_types[0], Type::I64 | Type::Bool | Type::Str) {
                Err("builtin 'panic' only supports i64, bool, or str".to_string())
            } else {
                Ok(Type::I64)
            }
        }
        "max" | "min" => {
            if arg_types == [Type::I64, Type::I64] {
                Ok(Type::I64)
            } else {
                Err(format!("builtin '{name}' expects exactly (i64, i64)"))
            }
        }
        "abs" => {
            if arg_types == [Type::I64] {
                Ok(Type::I64)
            } else {
                Err("builtin 'abs' expects exactly (i64)".to_string())
            }
        }
        "pow" => {
            if arg_types == [Type::I64, Type::I64] {
                Ok(Type::I64)
            } else {
                Err("builtin 'pow' expects exactly (i64, i64)".to_string())
            }
        }
        "clamp" => {
            if arg_types == [Type::I64, Type::I64, Type::I64] {
                Ok(Type::I64)
            } else {
                Err("builtin 'clamp' expects exactly (i64, i64, i64)".to_string())
            }
        }
        "str" => {
            if arg_types.len() != 1 {
                Err("builtin 'str' expects 1 argument".to_string())
            } else if !matches!(arg_types[0], Type::I64 | Type::Bool | Type::Str) {
                Err("builtin 'str' only supports i64, bool, or str".to_string())
            } else {
                Ok(Type::Str)
            }
        }
        "int" => {
            if arg_types.len() != 1 {
                Err("builtin 'int' expects 1 argument".to_string())
            } else if !matches!(arg_types[0], Type::I64 | Type::Bool | Type::Str) {
                Err("builtin 'int' only supports i64, bool, or str".to_string())
            } else {
                Ok(Type::I64)
            }
        }
        "bool" => {
            if arg_types.len() != 1 {
                Err("builtin 'bool' expects 1 argument".to_string())
            } else if !matches!(arg_types[0], Type::I64 | Type::Bool | Type::Str) {
                Err("builtin 'bool' only supports i64, bool, or str".to_string())
            } else {
                Ok(Type::Bool)
            }
        }
        "ord" => {
            if arg_types == [Type::Str] {
                Ok(Type::I64)
            } else {
                Err("builtin 'ord' expects exactly (str)".to_string())
            }
        }
        "chr" => {
            if arg_types == [Type::I64] {
                Ok(Type::Str)
            } else {
                Err("builtin 'chr' expects exactly (i64)".to_string())
            }
        }
        "contains" | "starts_with" | "ends_with" => {
            if arg_types == [Type::Str, Type::Str] {
                Ok(Type::Bool)
            } else {
                Err(format!("builtin '{name}' expects exactly (str, str)"))
            }
        }
        "find" => {
            if arg_types == [Type::Str, Type::Str] {
                Ok(Type::I64)
            } else {
                Err("builtin 'find' expects exactly (str, str)".to_string())
            }
        }
        "split" => {
            if arg_types == [Type::Str, Type::Str, Type::I64] {
                Ok(Type::Str)
            } else {
                Err("builtin 'split' expects exactly (str, str, i64)".to_string())
            }
        }
        "join" => {
            if arg_types == [Type::Str, Type::Str, Type::Str] {
                Ok(Type::Str)
            } else {
                Err("builtin 'join' expects exactly (str, str, str)".to_string())
            }
        }
        "replace" => {
            if arg_types == [Type::Str, Type::Str, Type::Str] {
                Ok(Type::Str)
            } else {
                Err("builtin 'replace' expects exactly (str, str, str)".to_string())
            }
        }
        "trim" | "upper" | "lower" => {
            if arg_types == [Type::Str] {
                Ok(Type::Str)
            } else {
                Err(format!("builtin '{name}' expects exactly (str)"))
            }
        }
        "exit" => {
            if arg_types == [Type::I64] {
                Ok(Type::I64)
            } else {
                Err("builtin 'exit' expects exactly (i64)".to_string())
            }
        }
        "ptr_can_read" | "ptr_can_write" => {
            if arg_types == [Type::I64, Type::I64] {
                Ok(Type::Bool)
            } else {
                Err(format!("builtin '{name}' expects exactly (i64, i64)"))
            }
        }
        "ptr_read8" | "ptr_read64" => {
            if arg_types == [Type::I64] {
                Ok(Type::I64)
            } else {
                Err(format!("builtin '{name}' expects exactly (i64)"))
            }
        }
        "ptr_write8" | "ptr_write64" => {
            if arg_types == [Type::I64, Type::I64] {
                Ok(Type::I64)
            } else {
                Err(format!("builtin '{name}' expects exactly (i64, i64)"))
            }
        }
        "ct_hash" => {
            if arg_types == [Type::Str] {
                Ok(Type::I64)
            } else {
                Err("builtin 'ct_hash' expects exactly (str)".to_string())
            }
        }
        "ct_xor" | "xor_decode" => {
            if arg_types == [Type::Str, Type::I64] {
                Ok(Type::Str)
            } else {
                Err(format!("builtin '{name}' expects exactly (str, i64)"))
            }
        }
        _ => return None,
    };
    Some(out)
}

pub fn ct_hash_str(input: &str) -> i64 {
    // 64-bit FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h as i64
}

pub fn ct_xor_hex(input: &str, key: i64) -> String {
    let k = (key & 0xff) as u8;
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.as_bytes() {
        let v = *b ^ k;
        out.push(hex_digit(v >> 4));
        out.push(hex_digit(v & 0x0f));
    }
    out
}

pub fn xor_decode_hex(input: &str, key: i64) -> Result<String, String> {
    if !input.len().is_multiple_of(2) {
        return Err("xor_decode expects an even-length hex string".to_string());
    }
    let k = (key & 0xff) as u8;
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_value(bytes[i])
            .ok_or_else(|| format!("xor_decode invalid hex digit '{}'", bytes[i] as char))?;
        let lo = hex_value(bytes[i + 1])
            .ok_or_else(|| format!("xor_decode invalid hex digit '{}'", bytes[i + 1] as char))?;
        out.push(((hi << 4) | lo) ^ k);
        i += 2;
    }
    String::from_utf8(out).map_err(|_| "xor_decode produced invalid UTF-8".to_string())
}

fn hex_digit(v: u8) -> char {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    HEX[(v & 0x0f) as usize] as char
}

fn hex_value(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(10 + ch - b'a'),
        b'A'..=b'F' => Some(10 + ch - b'A'),
        _ => None,
    }
}
