use std::time::Duration;

pub(super) fn render_diags(diags: Vec<xlang_compiler::diag::Diagnostic>) -> String {
    diags
        .into_iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn print_usage() {
    eprintln!("x <file.x> [--timings] [-- <args...>]");
    eprintln!("x check <file.x> [--timings]");
    eprintln!("x run <file.x> [--timings] [-- <args...>]");
    eprintln!("x debug <file.x> [--reload changed.x]");
    eprintln!("x build <file.x> [--dll|--lib] [--out output.dll|output.lib]");
}

pub(super) fn print_compile_timings(t: &xlang_compiler::pipeline::CompilerTimings) {
    println!("timings:");
    println!("  load: {}", format_duration(t.module_load));
    println!("  parse: {}", format_duration(t.parse));
    println!("  type-check: {}", format_duration(t.type_check));
    println!("  borrow-check: {}", format_duration(t.borrow_check));
    println!("  optimize: {}", format_duration(t.optimize));
    println!("  stdlib-closure: {}", format_duration(t.stdlib_closure));
    println!("  total: {}", format_duration(t.total));
}

pub(super) fn format_duration(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}
