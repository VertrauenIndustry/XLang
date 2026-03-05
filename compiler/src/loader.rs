use std::collections::{BTreeSet, HashMap, VecDeque};

#[derive(Debug, Clone)]
pub struct StdLibGraph {
    deps: HashMap<String, Vec<String>>,
}

impl Default for StdLibGraph {
    fn default() -> Self {
        let mut deps = HashMap::new();
        deps.insert("core::panic".to_string(), vec![]);
        deps.insert("core::fmt_i64".to_string(), vec!["core::panic".to_string()]);
        deps.insert(
            "core::io_print".to_string(),
            vec!["core::fmt_i64".to_string(), "core::panic".to_string()],
        );
        deps.insert(
            "core::io_input".to_string(),
            vec!["core::io_print".to_string(), "core::panic".to_string()],
        );
        deps.insert("core::env".to_string(), vec!["core::panic".to_string()]);
        deps.insert("core::time".to_string(), vec!["core::panic".to_string()]);
        deps.insert("core::thread".to_string(), vec!["core::panic".to_string()]);
        deps.insert("core::str_len".to_string(), vec!["core::panic".to_string()]);
        deps.insert(
            "core::clock_ms".to_string(),
            vec!["core::panic".to_string()],
        );
        deps.insert("core::math".to_string(), vec!["core::panic".to_string()]);
        deps.insert(
            "core::convert".to_string(),
            vec!["core::fmt_i64".to_string(), "core::panic".to_string()],
        );
        deps.insert(
            "core::str_utils".to_string(),
            vec!["core::panic".to_string()],
        );
        deps.insert("core::ffi".to_string(), vec!["core::panic".to_string()]);
        deps.insert(
            "net::http_serve".to_string(),
            vec!["core::panic".to_string(), "core::fmt_i64".to_string()],
        );
        deps.insert(
            "net::http_parse".to_string(),
            vec!["core::panic".to_string()],
        );
        deps.insert(
            "collections::vec_push".to_string(),
            vec!["core::panic".to_string()],
        );
        Self { deps }
    }
}

impl StdLibGraph {
    pub fn reachable(&self, roots: &[String]) -> BTreeSet<String> {
        let mut included = BTreeSet::new();
        let mut queue: VecDeque<String> = roots.iter().cloned().collect();

        while let Some(fn_name) = queue.pop_front() {
            if !included.insert(fn_name.clone()) {
                continue;
            }
            if let Some(children) = self.deps.get(&fn_name) {
                for child in children {
                    queue.push_back(child.clone());
                }
            }
        }
        included
    }
}
