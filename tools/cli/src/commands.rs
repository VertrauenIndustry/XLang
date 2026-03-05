use std::path::{Path, PathBuf};

use xlang_compiler::library_build::LibraryKind;
use xlang_compiler::pipeline::{compile_file, run_program_with_report, CompileOptions, RuntimeBackend};

use crate::output::{format_duration, print_compile_timings};

pub(super) fn run_check(file: &str, show_timings: bool) -> Result<i64, String> {
    match compile_file(Path::new(file), &CompileOptions::default()) {
        Ok(artifacts) => {
            println!("check ok");
            println!("reachable stdlib symbols:");
            for symbol in artifacts.reachable_stdlib {
                println!("  {symbol}");
            }
            if show_timings {
                print_compile_timings(&artifacts.timings);
            }
            Ok(0)
        }
        Err(diags) => {
            for d in diags {
                eprintln!("{d}");
            }
            Ok(2)
        }
    }
}

pub(super) fn run_file(file: &str, show_timings: bool) -> Result<i64, String> {
    let artifacts = compile_file(Path::new(file), &CompileOptions::default())
        .map_err(crate::output::render_diags)?;
    let result = run_program_with_report(&artifacts)?;
    println!("program exit code: {}", result.exit_code);
    if show_timings {
        print_compile_timings(&artifacts.timings);
        println!("execution: {}", format_duration(result.execution));
        println!(
            "backend: {}",
            match result.backend {
                RuntimeBackend::Native => "native",
                RuntimeBackend::Interpreter => "interpreter",
            }
        );
    }
    Ok(0)
}

pub(super) fn default_build_output(input: &str, kind: LibraryKind) -> PathBuf {
    let path = Path::new(input);
    match kind {
        LibraryKind::Dll => path.with_extension("dll"),
        LibraryKind::StaticLib => path.with_extension("lib"),
    }
}
