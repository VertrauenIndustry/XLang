use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::ast::{Expr, Program};
use crate::borrowck::borrow_check;
use crate::codegen::{run as run_interpreter, RunResult};
use crate::codegen_cranelift::run_native;
use crate::comptime::lower_comptime;
use crate::diag::Diagnostic;
use crate::loader::StdLibGraph;
use crate::opt::optimize;
use crate::parser::parse_program;
use crate::typecheck::{type_check, Signature};

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub optimize: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { optimize: true }
    }
}

#[derive(Debug, Clone)]
pub struct Artifacts {
    pub program: Program,
    pub signatures: HashMap<String, Signature>,
    pub reachable_stdlib: BTreeSet<String>,
    pub timings: CompilerTimings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerTimings {
    pub module_load: Duration,
    pub parse: Duration,
    pub type_check: Duration,
    pub borrow_check: Duration,
    pub optimize: Duration,
    pub stdlib_closure: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBackend {
    Native,
    Interpreter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub exit_code: i64,
    pub backend: RuntimeBackend,
    pub execution: Duration,
}

#[derive(Debug, Clone, Default)]
struct LoadStats {
    io: Duration,
    parse: Duration,
}

pub fn compile_source(
    source: &str,
    options: &CompileOptions,
) -> Result<Artifacts, Vec<Diagnostic>> {
    let total_start = Instant::now();
    let parse_start = Instant::now();
    let parsed = parse_program(source)?;
    let parse_time = parse_start.elapsed();
    if !parsed.imports.is_empty() {
        let mut diags = Vec::new();
        for import in &parsed.imports {
            diags.push(Diagnostic::new(
                import.line,
                format!(
                    "import '{}' requires file-based compilation; use `compile_file`/`x <file.x>`",
                    import.path
                ),
            ));
        }
        return Err(diags);
    }
    compile_program(
        parsed,
        options,
        LoadStats {
            parse: parse_time,
            ..LoadStats::default()
        },
        total_start,
    )
}

pub fn compile_file(path: &Path, options: &CompileOptions) -> Result<Artifacts, Vec<Diagnostic>> {
    let mut loaded = HashSet::new();
    let mut stack = Vec::new();
    let mut stats = LoadStats::default();
    let total_start = Instant::now();
    let program = load_program_recursive(path, &mut loaded, &mut stack, &mut stats)?;
    compile_program(program, options, stats, total_start)
}

fn compile_program(
    parsed: Program,
    options: &CompileOptions,
    stats: LoadStats,
    total_start: Instant,
) -> Result<Artifacts, Vec<Diagnostic>> {
    let parsed = lower_comptime(&parsed)?;
    let typecheck_start = Instant::now();
    let signatures = type_check(&parsed)?;
    let type_check = typecheck_start.elapsed();

    let borrow_start = Instant::now();
    borrow_check(&parsed, &signatures)?;
    let borrow_check = borrow_start.elapsed();

    let opt_start = Instant::now();
    let program = if options.optimize {
        optimize(&parsed)
    } else {
        parsed
    };
    let optimize = opt_start.elapsed();

    let stdlib_start = Instant::now();
    let roots = derive_stdlib_roots(&program);
    let reachable_stdlib = StdLibGraph::default().reachable(&roots);
    let stdlib_closure = stdlib_start.elapsed();

    Ok(Artifacts {
        program,
        signatures,
        reachable_stdlib,
        timings: CompilerTimings {
            module_load: stats.io,
            parse: stats.parse,
            type_check,
            borrow_check,
            optimize,
            stdlib_closure,
            total: total_start.elapsed(),
        },
    })
}

fn load_program_recursive(
    path: &Path,
    loaded: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    stats: &mut LoadStats,
) -> Result<Program, Vec<Diagnostic>> {
    let io_start = Instant::now();
    let canonical = fs::canonicalize(path).map_err(|e| {
        vec![Diagnostic::new(
            0,
            format!("failed to resolve module '{}': {e}", path.display()),
        )]
    })?;
    stats.io += io_start.elapsed();

    if loaded.contains(&canonical) {
        return Ok(Program {
            functions: Vec::new(),
            externs: Vec::new(),
            imports: Vec::new(),
        });
    }

    if let Some(pos) = stack.iter().position(|p| p == &canonical) {
        let mut chain = stack[pos..]
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>();
        chain.push(canonical.display().to_string());
        return Err(vec![Diagnostic::new(
            0,
            format!("cyclic import detected: {}", chain.join(" -> ")),
        )]);
    }

    let io_start = Instant::now();
    let source = fs::read_to_string(&canonical).map_err(|e| {
        vec![Diagnostic::new(
            0,
            format!("failed to read module '{}': {e}", canonical.display()),
        )]
    })?;
    stats.io += io_start.elapsed();

    stack.push(canonical.clone());
    let parse_start = Instant::now();
    let parsed = parse_program(&source).map_err(|diags| prefix_diags(diags, &canonical))?;
    stats.parse += parse_start.elapsed();

    let Program {
        functions: parsed_functions,
        externs: parsed_externs,
        imports,
    } = parsed;

    let mut functions = Vec::new();
    let mut externs = Vec::new();

    for import in imports {
        let imported_path = canonical
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&import.path);
        let imported = load_program_recursive(&imported_path, loaded, stack, stats)?;
        functions.extend(imported.functions);
        externs.extend(imported.externs);
    }

    functions.extend(parsed_functions);
    externs.extend(parsed_externs);

    stack.pop();
    loaded.insert(canonical);

    Ok(Program {
        functions,
        externs,
        imports: Vec::new(),
    })
}

fn prefix_diags(diags: Vec<Diagnostic>, path: &Path) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .map(|d| Diagnostic::new(d.line, format!("{}: {}", path.display(), d.message)))
        .collect()
}

pub fn run_program(artifacts: &Artifacts) -> Result<RunResult, String> {
    let report = run_program_with_report(artifacts)?;
    Ok(RunResult {
        exit_code: report.exit_code,
    })
}

pub fn run_program_with_report(artifacts: &Artifacts) -> Result<RunReport, String> {
    let exec_start = Instant::now();
    if program_has_threading(&artifacts.program) {
        let out = run_interpreter(&artifacts.program)?;
        return Ok(RunReport {
            exit_code: out.exit_code,
            backend: RuntimeBackend::Interpreter,
            execution: exec_start.elapsed(),
        });
    }
    let exit_code = run_native(&artifacts.program)?;
    Ok(RunReport {
        exit_code,
        backend: RuntimeBackend::Native,
        execution: exec_start.elapsed(),
    })
}

fn program_has_threading(program: &Program) -> bool {
    for f in &program.functions {
        for stmt in &f.body {
            if stmt_has_threading(stmt) {
                return true;
            }
        }
    }
    false
}

fn stmt_has_threading(stmt: &crate::ast::Stmt) -> bool {
    match stmt {
        crate::ast::Stmt::ThreadCall { .. } | crate::ast::Stmt::ThreadWhile { .. } => true,
        crate::ast::Stmt::IfIs {
            arms, else_body, ..
        } => {
            for arm in arms {
                for s in &arm.body {
                    if stmt_has_threading(s) {
                        return true;
                    }
                }
            }
            for s in else_body {
                if stmt_has_threading(s) {
                    return true;
                }
            }
            false
        }
        crate::ast::Stmt::If {
            then_body,
            elif_arms,
            else_body,
            ..
        } => {
            for s in then_body {
                if stmt_has_threading(s) {
                    return true;
                }
            }
            for arm in elif_arms {
                for s in &arm.body {
                    if stmt_has_threading(s) {
                        return true;
                    }
                }
            }
            for s in else_body {
                if stmt_has_threading(s) {
                    return true;
                }
            }
            false
        }
        crate::ast::Stmt::While { body, .. }
        | crate::ast::Stmt::ForRange { body, .. }
        | crate::ast::Stmt::Comptime { body, .. } => body.iter().any(stmt_has_threading),
        _ => false,
    }
}

fn derive_stdlib_roots(program: &Program) -> Vec<String> {
    let mut roots = Vec::new();
    for f in &program.functions {
        for stmt in &f.body {
            collect_stmt_roots(stmt, &mut roots);
        }
    }
    if roots.is_empty() {
        roots.push("core::panic".to_string());
    }
    roots
}

fn collect_stmt_roots(stmt: &crate::ast::Stmt, roots: &mut Vec<String>) {
    match stmt {
        crate::ast::Stmt::Let { expr, .. }
        | crate::ast::Stmt::Assign { expr, .. }
        | crate::ast::Stmt::Return { expr, .. }
        | crate::ast::Stmt::Expr { expr, .. } => collect_expr_roots(expr, roots),
        crate::ast::Stmt::IfIs {
            value,
            arms,
            else_body,
            ..
        } => {
            collect_expr_roots(value, roots);
            for arm in arms {
                for pattern in &arm.patterns {
                    collect_is_pattern_roots(pattern, roots);
                }
                for stmt in &arm.body {
                    collect_stmt_roots(stmt, roots);
                }
            }
            for stmt in else_body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::If {
            condition,
            then_body,
            elif_arms,
            else_body,
            ..
        } => {
            collect_expr_roots(condition, roots);
            for stmt in then_body {
                collect_stmt_roots(stmt, roots);
            }
            for arm in elif_arms {
                collect_expr_roots(&arm.condition, roots);
                for stmt in &arm.body {
                    collect_stmt_roots(stmt, roots);
                }
            }
            for stmt in else_body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::While {
            condition, body, ..
        } => {
            collect_expr_roots(condition, roots);
            for stmt in body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::ThreadWhile {
            condition,
            body,
            count,
            ..
        } => {
            collect_expr_roots(condition, roots);
            collect_expr_roots(count, roots);
            roots.push("core::thread".to_string());
            for stmt in body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            collect_expr_roots(start, roots);
            collect_expr_roots(end, roots);
            if let Some(step) = step {
                collect_expr_roots(step, roots);
            }
            for stmt in body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::ThreadCall { call, count, .. } => {
            collect_expr_roots(call, roots);
            collect_expr_roots(count, roots);
            roots.push("core::thread".to_string());
        }
        crate::ast::Stmt::Comptime { body, .. } => {
            for stmt in body {
                collect_stmt_roots(stmt, roots);
            }
        }
        crate::ast::Stmt::Pass { .. }
        | crate::ast::Stmt::Break { .. }
        | crate::ast::Stmt::Continue { .. } => {}
    }
}

fn collect_is_pattern_roots(pattern: &crate::ast::IsPattern, roots: &mut Vec<String>) {
    match pattern {
        crate::ast::IsPattern::Value(expr)
        | crate::ast::IsPattern::Ne(expr)
        | crate::ast::IsPattern::Lt(expr)
        | crate::ast::IsPattern::Le(expr)
        | crate::ast::IsPattern::Gt(expr)
        | crate::ast::IsPattern::Ge(expr)
        | crate::ast::IsPattern::StartsWith(expr)
        | crate::ast::IsPattern::EndsWith(expr)
        | crate::ast::IsPattern::Contains(expr) => collect_expr_roots(expr, roots),
        crate::ast::IsPattern::Range { start, end } => {
            collect_expr_roots(start, roots);
            collect_expr_roots(end, roots);
        }
    }
}

fn collect_expr_roots(expr: &Expr, roots: &mut Vec<String>) {
    match expr {
        Expr::Call { name, args } => {
            if let Some(root) = builtin_root(name) {
                roots.push(root.to_string());
            }
            if name.contains("::") {
                roots.push(name.clone());
            }
            for arg in args {
                collect_expr_roots(arg, roots);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_roots(left, roots);
            collect_expr_roots(right, roots);
        }
        Expr::Unary { expr, .. } => collect_expr_roots(expr, roots),
        _ => {}
    }
}

fn builtin_root(name: &str) -> Option<&'static str> {
    match name {
        "print" | "write" | "println" => Some("core::io_print"),
        "input" => Some("core::io_input"),
        "argc" | "argv" => Some("core::env"),
        "sleep_ms" => Some("core::time"),
        "len" => Some("core::str_len"),
        "clock_ms" => Some("core::clock_ms"),
        "assert" | "panic" | "exit" => Some("core::panic"),
        "max" | "min" | "abs" | "pow" | "clamp" => Some("core::math"),
        "str" | "int" | "bool" => Some("core::convert"),
        "ord" | "chr" => Some("core::str_utils"),
        "contains" | "find" | "starts_with" | "ends_with" | "replace" | "split" | "join"
        | "trim" | "upper" | "lower" => {
            Some("core::str_utils")
        }
        "ptr_can_read" | "ptr_can_write" | "ptr_read8" | "ptr_write8" | "ptr_read64"
        | "ptr_write64" => Some("core::ffi"),
        _ => None,
    }
}
