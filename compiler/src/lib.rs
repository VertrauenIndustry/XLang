pub mod ast;
pub mod borrowck;
pub mod builtins;
pub mod codegen;
pub mod codegen_cranelift;
pub mod comptime;
pub mod debug;
pub mod diag;
pub mod library_build;
pub mod loader;
pub mod memory;
pub mod opt;
pub mod parser;
pub mod pipeline;
pub mod runtime_args;
pub mod typecheck;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::codegen_cranelift::{build_native_image, plan_native_patch, run_native};
    use crate::debug::DebugSession;
    use crate::library_build::emit_rust_library_source;
    use crate::loader::StdLibGraph;
    use crate::opt::optimize;
    use crate::parser::parse_program;
    use crate::pipeline::{
        compile_file, compile_source, run_program, run_program_with_report, CompileOptions,
    };

    const HELLO: &str = r#"
fn main() -> i64:
    let x: i64 = 40 + 2
    return x
"#;

    #[test]
    fn parses_and_runs() {
        let artifacts = compile_source(HELLO, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn catches_move_after_use() {
        let src = r#"
fn sink(v: str) -> i64:
    return 0

fn main() -> i64:
    let name: str = "xlang"
    sink(move name)
    sink(name)
    return 0
"#;

        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("use after move"));
    }

    #[test]
    fn selective_loader_closure() {
        let graph = StdLibGraph::default();
        let reachable = graph.reachable(&["net::http_serve".to_string()]);
        assert!(reachable.contains("net::http_serve"));
        assert!(reachable.contains("core::panic"));
        assert!(reachable.contains("core::fmt_i64"));
        assert!(!reachable.contains("collections::vec_push"));
    }

    #[test]
    fn debug_reload_detects_body_change_without_restart() {
        let v1 = parse_program(
            r#"
fn main() -> i64:
    return 1
"#,
        )
        .unwrap();
        let v2 = parse_program(
            r#"
fn main() -> i64:
    return 2
"#,
        )
        .unwrap();
        let mut session = DebugSession::from_program(&v1);
        let delta = session.reload(&v2);
        assert_eq!(delta.recompiled_functions, vec!["main".to_string()]);
        assert!(!delta.restart_required);
    }

    #[test]
    fn debug_reload_requires_restart_on_signature_change() {
        let v1 = parse_program(
            r#"
fn main() -> i64:
    return 1
"#,
        )
        .unwrap();
        let v2 = parse_program(
            r#"
fn main() -> bool:
    return true
"#,
        )
        .unwrap();
        let mut session = DebugSession::from_program(&v1);
        let delta = session.reload(&v2);
        assert!(delta.restart_required);
    }

    #[test]
    fn native_patch_allows_body_only_change() {
        let v1 = parse_program(
            r#"
fn main() -> i64:
    return 1
"#,
        )
        .unwrap();
        let v2 = parse_program(
            r#"
fn main() -> i64:
    return 2
"#,
        )
        .unwrap();

        let old_image = build_native_image(&v1).expect("old image");
        let new_image = build_native_image(&v2).expect("new image");
        let patch = plan_native_patch(&old_image, &new_image);
        assert_eq!(patch.patched_functions, vec!["main".to_string()]);
        assert!(!patch.restart_required);
    }

    #[test]
    fn native_patch_rejects_abi_change() {
        let v1 = parse_program(
            r#"
fn foo() -> i64:
    return 1

fn main() -> i64:
    return 0
"#,
        )
        .unwrap();
        let v2 = parse_program(
            r#"
fn foo() -> bool:
    return true

fn main() -> i64:
    return 0
"#,
        )
        .unwrap();

        let old_image = build_native_image(&v1).expect("old image");
        let new_image = build_native_image(&v2).expect("new image");
        let patch = plan_native_patch(&old_image, &new_image);
        assert!(patch.rejected_functions.contains(&"foo".to_string()));
        assert!(patch.restart_required);
    }

    #[test]
    fn builtins_program_executes() {
        let src = r#"
fn main() -> i64:
    let s: str = str(42)
    print(s)
    println()
    let n: i64 = int(s)
    let m: i64 = max(n, min(100, 40 + 2))
    let a: i64 = abs(0 - 7)
    return m + a
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 49);
    }

    #[test]
    fn friendly_syntax_without_explicit_return_types() {
        let src = r#"
function add(a: i64, b: i64):
    return a + b

def main():
    result = add(20, 22)
    print("Result: ")
    print(result)
    println()
    return result
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn input_builtin_typechecks() {
        let src = r#"
fn main() -> i64:
    let name = input(">> ")
    if len(name) >= 0:
        return 1
    return 0
"#;
        let _ = compile_source(src, &CompileOptions::default()).expect("compile");
    }

    #[test]
    fn input_builtin_rejects_non_string_prompt() {
        let src = r#"
fn main() -> i64:
    let name = input(42)
    return len(name)
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("builtin 'input' expects () or (str)"));
    }

    #[test]
    fn cli_core_builtins_execute() {
        let src = r#"
fn main() -> i64:
    let c = argc()
    let a0 = argv(0)
    sleep_ms(0)
    if c >= 0 and len(a0) >= 0:
        return 1
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn argv_builtin_rejects_non_i64_index() {
        let src = r#"
fn main() -> i64:
    let v = argv("x")
    return len(v)
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("builtin 'argv' expects exactly (i64)"));
    }

    #[test]
    fn if_is_statement_executes() {
        let src = r#"
def main():
    yes = 3
    if (yes):
        is 2:
            return 20
        is 3:
            return 30
        is 4:
            return 40
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 30);
    }

    #[test]
    fn parses_extern_declaration() {
        let program = parse_program(
            r#"
extern fn c_abs(v: i64) -> i64 from "msvcrt.dll"

fn main() -> i64:
    return 0
"#,
        )
        .expect("parse");
        assert_eq!(program.externs.len(), 1);
        assert_eq!(program.externs[0].name, "c_abs");
    }

    #[test]
    fn emits_library_source() {
        let program = parse_program(
            r#"
fn add(a: i64, b: i64) -> i64:
    return a + b

fn main() -> i64:
    return add(1, 2)
"#,
        )
        .expect("parse");
        let src = emit_rust_library_source(&program).expect("emit");
        assert!(src.contains("pub extern \"C\" fn add"));
        assert!(src.contains("pub extern \"C\" fn main"));
    }

    #[test]
    fn if_is_without_colon_on_if_line_executes() {
        let src = r#"
fn main() -> i64:
    let e = 42
    if e
        is 2:
            return 10
        is 42:
            return e
    return e
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn if_is_matches_string_multi_patterns() {
        let src = r#"
fn main() -> i64:
    let tag = "green"
    if tag
        is "red", "green", "blue":
            return 7
        is "black":
            return 9
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 7);
    }

    #[test]
    fn if_is_default_aliases_work() {
        let src = r#"
fn a() -> i64:
    let v = 9
    if v
        is 1:
            return 11
        is default:
            return 22
    return 0

fn b() -> i64:
    let v = 9
    if v
        is 1:
            return 11
        is none:
            return 33
    return 0

fn c() -> i64:
    let v = 9
    if v
        is 1:
            return 11
        is null:
            return 44
    return 0

fn main() -> i64:
    return a() + b() + c()
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 99);
    }

    #[test]
    fn if_is_string_predicates_execute() {
        let src = r#"
fn main() -> i64:
    let tag = "alpha-beta"
    if tag
        is starts_with "alpha":
            return 10
        is contains "beta":
            return 20
        is ends_with "z":
            return 30
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 10);
    }

    #[test]
    fn if_is_numeric_predicates_execute() {
        let src = r#"
fn main() -> i64:
    let n = 15
    if n
        is < 0:
            return 1
        is 10..20:
            return 2
        is >= 100:
            return 3
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 2);
    }

    #[test]
    fn control_flow_if_while_break_continue_executes() {
        let src = r#"
fn main() -> i64:
    let i = 0
    let acc = 0
    while i - 5:
        i = i + 1
        if i
            is 2:
                continue
            is 4:
                break
        if i:
            acc = acc + i
        elif false:
            acc = acc + 100
        else:
            acc = acc + 1000
    return acc
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 4);
    }

    #[test]
    fn break_outside_loop_fails() {
        let src = r#"
fn main() -> i64:
    break
    return 0
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("`break` can only appear inside a loop"));
    }

    #[test]
    fn module_imports_compile_from_file() {
        let dir = unique_tmp_dir("x_mod_import");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(
            dir.join("math.x"),
            r#"
fn add(a: i64, b: i64) -> i64:
    return a + b
"#,
        )
        .expect("write math");
        fs::write(
            dir.join("main.x"),
            r#"
import "math.x"

fn main() -> i64:
    return add(20, 22)
"#,
        )
        .expect("write main");

        let artifacts =
            compile_file(&dir.join("main.x"), &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn module_import_cycle_is_reported() {
        let dir = unique_tmp_dir("x_mod_cycle");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(
            dir.join("a.x"),
            r#"
import "b.x"

fn fa() -> i64:
    return 1
"#,
        )
        .expect("write a");
        fs::write(
            dir.join("b.x"),
            r#"
import "a.x"

fn fb() -> i64:
    return 2
"#,
        )
        .expect("write b");
        fs::write(
            dir.join("main.x"),
            r#"
import "a.x"

fn main() -> i64:
    return 0
"#,
        )
        .expect("write main");

        let err =
            compile_file(&dir.join("main.x"), &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("cyclic import detected"));
    }

    #[test]
    fn expanded_string_math_builtins_execute() {
        let src = r#"
fn main() -> i64:
    let s = "  HeLLo world  "
    let t = trim(lower(s))
    if contains(t, "hello"):
        if starts_with(t, "hello"):
            if ends_with(t, "world"):
                let r = replace(t, "world", "x")
                if r
                    is "hello x":
                        let p = pow(2, 5)
                        return clamp(p, 0, 20)
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 20);
    }

    #[test]
    fn bool_ord_chr_builtins_execute() {
        let src = r#"
fn main() -> i64:
    let a = bool(0)
    let b = bool("x")
    let code = ord("A")
    let ch = chr(65)
    if not a and b and code == 65 and ch == "A":
        return 1
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn string_find_split_join_builtins_execute() {
        let src = r#"
fn main() -> i64:
    let idx = find("alpha_beta", "_")
    let part = split("alpha_beta", "_", 1)
    let merged = join("alpha", "beta", "_")
    if idx == 5 and part == "beta" and merged == "alpha_beta":
        return 1
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn method_call_sugar_executes() {
        let src = r#"
fn main() -> i64:
    let s = "  a,b,c  "
    let part = s.trim().split(",", 1)
    let idx = part.find("b")
    let out = "left".join("right", "-")
    if idx == 0 and out == "left-right":
        return exit(7)
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 7);
    }

    #[test]
    fn pointer_capability_builtins_execute() {
        let src = r#"
fn main() -> i64:
    if not ptr_can_read(0, 8) and not ptr_can_write(0, 8):
        return 7
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 7);
    }

    #[test]
    fn invalid_pointer_access_is_non_crashing() {
        let src = r#"
fn main() -> i64:
    let a = ptr_read8(0)
    let b = ptr_read64(0)
    let w1 = ptr_write8(0, 1)
    let w2 = ptr_write64(0, 2)
    if a == 0 and b == 0 and w1 == -1 and w2 == -1:
        return 1
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn native_backend_executes_standard_control_flow() {
        let program = parse_program(
            r#"
fn main() -> i64:
    let i = 0
    let acc = 0
    while i - 10:
        i = i + 1
        if i:
            if i - 7:
                acc = acc + i
            else:
                break
    return acc
"#,
        )
        .expect("parse");
        let out = run_native(&program).expect("native run");
        assert_eq!(out, 21);
    }

    #[test]
    fn for_range_executes() {
        let src = r#"
fn main() -> i64:
    let sum = 0
    for i in 1..6:
        sum = sum + i
    return sum
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 15);
    }

    #[test]
    fn for_range_continue_and_break_executes() {
        let src = r#"
fn main() -> i64:
    let sum = 0
    for i in 0..10:
        if i
            is 2:
                continue
            is 7:
                break
        sum = sum + i
    return sum
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 19);
    }

    #[test]
    fn native_backend_executes_for_range() {
        let program = parse_program(
            r#"
fn main() -> i64:
    let sum = 0
    for i in 1..5:
        sum = sum + i
    return sum
"#,
        )
        .expect("parse");
        let out = run_native(&program).expect("native run");
        assert_eq!(out, 10);
    }

    #[test]
    fn native_backend_executes_if_is_numeric() {
        let program = parse_program(
            r#"
fn main() -> i64:
    let n = 15
    if n
        is < 0:
            return 1
        is 10..20:
            return 2
        is >= 100:
            return 3
    return 0
"#,
        )
        .expect("parse");
        let out = run_native(&program).expect("native run");
        assert_eq!(out, 2);
    }

    #[test]
    fn run_report_errors_when_native_backend_rejects_program() {
        let src = r#"
extern fn c_unused(v: i64) -> i64 from "msvcrt.dll"

fn main() -> i64:
    return 42
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let err = run_program_with_report(&artifacts).expect_err("must fail");
        assert!(err.contains("extern imports"));
    }

    #[test]
    fn comparisons_logic_and_unary_execute() {
        let src = r#"
fn main() -> i64:
    let a = 5
    let b = -3 + 8
    if (a == b) and not (a != b):
        if (a < 10) and (a <= 5) and (a > 4) and (a >= 5):
            return 42
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn string_equality_executes() {
        let src = r#"
fn main() -> i64:
    let s = "xlang"
    if s == "xlang":
        return 7
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 7);
    }

    #[test]
    fn comments_and_compound_assignment_execute() {
        let src = r#"
fn main() -> i64:
    let x = 1 // seed
    x += 2 # plus
    x *= 3
    x -= 1
    x /= 2
    let url = "http://x//y#z"
    if url == "http://x//y#z":
        return x
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 4);
    }

    #[test]
    fn tilde_brace_inline_comment_executes() {
        let src = r#"
fn main() -> i64:
    let x = 2 ~ { this should be ignored } ~ + 40
    return x
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn tilde_brace_multiline_comment_executes() {
        let src = r#"
fn main() -> i64:
    ~ {
      multiline
      comment
    } ~
    return 42
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn unterminated_tilde_brace_comment_is_reported() {
        let src = r#"
fn main() -> i64:
    let x = 1
    ~ { broken
    return x
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("unterminated ~ { ... } ~ comment"));
    }

    #[test]
    fn variadic_print_and_println_execute() {
        let src = r#"
fn main() -> i64:
    print("A", 1, true)
    println("B", 2, false)
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn pass_statement_executes() {
        let src = r#"
fn main() -> i64:
    pass
    return 42
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn for_range_with_step_executes() {
        let src = r#"
fn main() -> i64:
    let s1 = 0
    for i in 0..10 step 3:
        s1 += i

    let s2 = 0
    for j in 10..0 step -4:
        s2 += j
    return s1 + s2
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 36);
    }

    #[test]
    fn thread_call_wait_executes() {
        let src = r#"
fn worker(v: i64) -> i64:
    sleep_ms(1)
    return v + 1

fn main() -> i64:
    worker(10) -> thread(4).wait()
    return 42
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 42);
    }

    #[test]
    fn thread_while_wait_executes() {
        let src = r#"
fn main() -> i64:
    let n = 3
    while n: -> thread(2).wait()
        n -= 1
    return 7
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 7);
    }

    #[test]
    fn thread_count_requires_i64() {
        let src = r#"
fn worker() -> i64:
    return 0

fn main() -> i64:
    worker() -> thread("x")
    return 0
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("thread count must be i64"));
    }

    #[test]
    fn for_range_step_zero_fails() {
        let src = r#"
fn main() -> i64:
    for i in 0..10 step 0:
        pass
    return 0
"#;
        let err = compile_source(src, &CompileOptions::default()).expect_err("must fail");
        let joined = err
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("for-range step cannot be 0"));
    }

    #[test]
    fn modulo_and_string_concat_execute() {
        let src = r#"
fn main() -> i64:
    let msg = "Hello, " + "world"
    if msg == "Hello, world":
        return 43 % 10
    return 0
"#;
        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 3);
    }

    #[test]
    fn compile_time_builtins_fold_and_execute() {
        let src = r#"
fn main() -> i64:
    let h = ct_hash("xlang")
    let d = xor_decode(ct_xor("Hi", 23), 23)
    if h != 0:
        if d == "Hi":
            return 1
    return 0
"#;
        let parsed = parse_program(src).expect("parse");
        let optimized = optimize(&parsed);
        let main = optimized.function("main").expect("main");
        match &main.body[0] {
            crate::ast::Stmt::Let { expr, .. } => {
                assert!(matches!(expr, crate::ast::Expr::Int(_)));
            }
            _ => panic!("expected folded let"),
        }
        match &main.body[1] {
            crate::ast::Stmt::Let { expr, .. } => {
                assert!(matches!(expr, crate::ast::Expr::Str(v) if v == "Hi"));
            }
            _ => panic!("expected folded string let"),
        }

        let artifacts = compile_source(src, &CompileOptions::default()).expect("compile");
        let out = run_program(&artifacts).expect("run");
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn optimizer_inlines_small_return_only_function() {
        let src = r#"
fn add1(x: i64) -> i64:
    return x + 1

fn main() -> i64:
    return add1(41)
"#;
        let parsed = parse_program(src).expect("parse");
        let optimized = optimize(&parsed);
        let main = optimized.function("main").expect("main");
        let crate::ast::Stmt::Return { expr, .. } = &main.body[0] else {
            panic!("main should return directly");
        };
        assert!(matches!(expr, crate::ast::Expr::Int(42)));
    }

    fn unique_tmp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}"))
    }
}
