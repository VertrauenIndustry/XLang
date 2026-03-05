use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    pub id: u64,
    pub bytes: usize,
}

#[derive(Debug, Default)]
pub struct Arena {
    next_id: u64,
    allocations: HashMap<u64, Allocation>,
}

impl Arena {
    pub fn alloc(&mut self, bytes: usize) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.allocations.insert(id, Allocation { id, bytes });
        id
    }

    pub fn dealloc(&mut self, id: u64) -> bool {
        self.allocations.remove(&id).is_some()
    }

    pub fn checkpoint(&self) -> BTreeSet<u64> {
        self.allocations.keys().copied().collect()
    }

    pub fn allocation_count(&self) -> usize {
        self.allocations.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRevision {
    pub name: String,
    pub revision: u64,
    pub code_ptr: usize,
    pub abi_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSlot {
    pub revision: u64,
    pub code_ptr: usize,
    pub abi_hash: u64,
}

#[derive(Debug, Default)]
pub struct FunctionTable {
    slots: HashMap<String, FunctionSlot>,
}

impl FunctionTable {
    pub fn seed(&mut self, name: &str, abi_hash: u64, code_ptr: usize) {
        self.slots.entry(name.to_string()).or_insert(FunctionSlot {
            revision: 0,
            code_ptr,
            abi_hash,
        });
    }

    pub fn patch_checked(
        &mut self,
        name: &str,
        abi_hash: u64,
        code_ptr: usize,
    ) -> Result<FunctionRevision, String> {
        let next = match self.slots.get(name) {
            Some(slot) => {
                if slot.abi_hash != abi_hash {
                    return Err(format!("ABI mismatch while patching '{name}'"));
                }
                slot.revision.saturating_add(1)
            }
            None => 1,
        };
        self.slots.insert(
            name.to_string(),
            FunctionSlot {
                revision: next,
                code_ptr,
                abi_hash,
            },
        );
        Ok(FunctionRevision {
            name: name.to_string(),
            revision: next,
            code_ptr,
            abi_hash,
        })
    }

    pub fn revision(&self, name: &str) -> Option<u64> {
        self.slots.get(name).map(|s| s.revision)
    }

    pub fn slot(&self, name: &str) -> Option<&FunctionSlot> {
        self.slots.get(name)
    }
}

#[derive(Debug, Default)]
pub struct RuntimeState {
    pub arena: Arena,
    pub fn_table: FunctionTable,
}

#[cfg(test)]
mod tests {
    use super::FunctionTable;

    #[test]
    fn patch_checked_accepts_same_abi() {
        let mut table = FunctionTable::default();
        table.seed("main", 7, 0x1000);
        let rev = table.patch_checked("main", 7, 0x2000).expect("patch");
        assert_eq!(rev.revision, 1);
        assert_eq!(table.revision("main"), Some(1));
    }

    #[test]
    fn patch_checked_rejects_abi_mismatch() {
        let mut table = FunctionTable::default();
        table.seed("main", 7, 0x1000);
        let err = table
            .patch_checked("main", 9, 0x2000)
            .expect_err("must fail");
        assert!(err.contains("ABI mismatch"));
    }
}
