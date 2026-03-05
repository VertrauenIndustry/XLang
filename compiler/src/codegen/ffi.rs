use std::collections::HashMap;

use libloading::Library;

use crate::ast::ExternFunction;

use super::Value;

#[derive(Default)]
pub(super) struct FfiRuntime {
    pub(super) externs: HashMap<String, ExternFunction>,
    libraries: HashMap<String, Library>,
}

impl FfiRuntime {
    pub(super) fn new(externs: HashMap<String, ExternFunction>) -> Self {
        Self {
            externs,
            libraries: HashMap::new(),
        }
    }
}

pub(super) fn eval_extern_call(
    name: &str,
    args: &[Value],
    ffi: &mut FfiRuntime,
) -> Result<Value, String> {
    let ext = ffi
        .externs
        .get(name)
        .cloned()
        .ok_or_else(|| format!("extern function '{name}' not declared"))?;
    if args.len() != ext.params.len() {
        return Err(format!(
            "extern function '{}' expected {} args, got {}",
            name,
            ext.params.len(),
            args.len()
        ));
    }
    let mut raw = Vec::with_capacity(args.len());
    for arg in args {
        raw.push(arg.as_i64()?);
    }
    let library = load_library(&ext.library, ffi)?;
    let result = unsafe { invoke_symbol_i64(library, &ext.name, &raw)? };
    Ok(Value::I64(result))
}

fn load_library<'a>(name: &str, ffi: &'a mut FfiRuntime) -> Result<&'a Library, String> {
    if !ffi.libraries.contains_key(name) {
        let lib = unsafe { Library::new(name) }
            .map_err(|e| format!("failed to load library '{name}': {e}"))?;
        ffi.libraries.insert(name.to_string(), lib);
    }
    ffi.libraries
        .get(name)
        .ok_or_else(|| format!("library '{name}' not loaded"))
}

unsafe fn invoke_symbol_i64(lib: &Library, symbol_name: &str, args: &[i64]) -> Result<i64, String> {
    let mut sym = symbol_name.as_bytes().to_vec();
    sym.push(0);
    match args.len() {
        0 => {
            let f: libloading::Symbol<unsafe extern "C" fn() -> i64> =
                lib.get(&sym).map_err(|e| e.to_string())?;
            Ok(f())
        }
        1 => {
            let f: libloading::Symbol<unsafe extern "C" fn(i64) -> i64> =
                lib.get(&sym).map_err(|e| e.to_string())?;
            Ok(f(args[0]))
        }
        2 => {
            let f: libloading::Symbol<unsafe extern "C" fn(i64, i64) -> i64> =
                lib.get(&sym).map_err(|e| e.to_string())?;
            Ok(f(args[0], args[1]))
        }
        3 => {
            let f: libloading::Symbol<unsafe extern "C" fn(i64, i64, i64) -> i64> =
                lib.get(&sym).map_err(|e| e.to_string())?;
            Ok(f(args[0], args[1], args[2]))
        }
        4 => {
            let f: libloading::Symbol<unsafe extern "C" fn(i64, i64, i64, i64) -> i64> =
                lib.get(&sym).map_err(|e| e.to_string())?;
            Ok(f(args[0], args[1], args[2], args[3]))
        }
        _ => Err("extern calls currently support up to 4 i64 arguments".to_string()),
    }
}
