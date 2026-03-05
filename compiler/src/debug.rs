use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::ast::{Function, Program, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionFingerprint {
    pub sig_hash: u64,
    pub body_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadResult {
    pub recompiled_functions: Vec<String>,
    pub restart_required: bool,
}

#[derive(Debug, Default)]
pub struct DebugSession {
    functions: HashMap<String, FunctionFingerprint>,
}

impl DebugSession {
    pub fn from_program(program: &Program) -> Self {
        let functions = program
            .functions
            .iter()
            .map(|f| (f.name.clone(), fingerprint(f)))
            .collect();
        Self { functions }
    }

    pub fn reload(&mut self, program: &Program) -> ReloadResult {
        let mut recompiled = Vec::new();
        let mut restart_required = false;

        for f in &program.functions {
            let new_fp = fingerprint(f);
            match self.functions.get(&f.name) {
                Some(old_fp) => {
                    if old_fp.body_hash != new_fp.body_hash {
                        recompiled.push(f.name.clone());
                    }
                    if old_fp.sig_hash != new_fp.sig_hash {
                        restart_required = true;
                    }
                }
                None => {
                    recompiled.push(f.name.clone());
                    restart_required = true;
                }
            }
            self.functions.insert(f.name.clone(), new_fp);
        }

        ReloadResult {
            recompiled_functions: recompiled,
            restart_required,
        }
    }
}

fn fingerprint(f: &Function) -> FunctionFingerprint {
    let mut sig_hasher = std::collections::hash_map::DefaultHasher::new();
    f.name.hash(&mut sig_hasher);
    for p in &f.params {
        p.name.hash(&mut sig_hasher);
        type_tag(&p.ty).hash(&mut sig_hasher);
    }
    type_tag(&f.ret).hash(&mut sig_hasher);

    let mut body_hasher = std::collections::hash_map::DefaultHasher::new();
    f.body.hash(&mut body_hasher);

    FunctionFingerprint {
        sig_hash: sig_hasher.finish(),
        body_hash: body_hasher.finish(),
    }
}

fn type_tag(ty: &Type) -> u8 {
    match ty {
        Type::Infer => 0,
        Type::I64 => 1,
        Type::Bool => 2,
        Type::Str => 3,
        Type::Void => 4,
    }
}
