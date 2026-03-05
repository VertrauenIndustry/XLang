use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod commands;
mod output;

use commands::{default_build_output, run_check, run_file};
use output::{print_usage, render_diags};
use xlang_compiler::codegen_cranelift::{build_native_image, plan_native_patch};
use xlang_compiler::debug::DebugSession;
use xlang_compiler::library_build::{build_library, LibraryKind};
use xlang_compiler::pipeline::{compile_file, CompileOptions};
use xlang_runtime::RuntimeState;

fn main() -> ExitCode {
    match run_cli() {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run_cli() -> Result<i64, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return Ok(1);
    }

    let cmd = args[0].clone();
    if cmd.ends_with(".x") {
        let mut show_timings = false;
        for flag in args.iter().skip(1) {
            if flag == "--timings" {
                show_timings = true;
            }
        }
        return run_file(&cmd, show_timings);
    }

    let mut sub_args = args.into_iter().skip(1);

    match cmd.as_str() {
        "check" => {
            let file = sub_args
                .next()
                .ok_or_else(|| "missing file path".to_string())?;
            let mut show_timings = false;
            for flag in sub_args.by_ref() {
                match flag.as_str() {
                    "--timings" => show_timings = true,
                    _ => return Err(format!("unknown check flag '{flag}'")),
                }
            }
            run_check(&file, show_timings)
        }
        "run" => {
            let file = sub_args
                .next()
                .ok_or_else(|| "missing file path".to_string())?;
            let mut show_timings = false;
            for flag in sub_args.by_ref() {
                if flag == "--timings" {
                    show_timings = true;
                }
            }
            run_file(&file, show_timings)
        }
        "debug" => {
            let file = sub_args
                .next()
                .ok_or_else(|| "missing file path".to_string())?;
            let artifacts =
                compile_file(Path::new(&file), &CompileOptions::default()).map_err(render_diags)?;
            let base_image = build_native_image(&artifacts.program);

            let mut runtime = RuntimeState::default();
            let alloc_a = runtime.arena.alloc(64);
            let alloc_b = runtime.arena.alloc(128);
            if let Ok(native) = &base_image {
                for (name, abi_hash) in &native.abi_hashes {
                    let code_ptr = *native.code_ptrs.get(name).unwrap_or(&0);
                    runtime.fn_table.seed(name, *abi_hash, code_ptr);
                }
            }
            println!(
                "debug session started (allocations: {} => [{alloc_a}, {alloc_b}])",
                runtime.arena.allocation_count()
            );
            if let Err(err) = &base_image {
                println!("debug backend fallback (native patching unavailable): {err}");
            }

            let mut reload_path = None;
            while let Some(flag) = sub_args.next() {
                if flag == "--reload" {
                    reload_path = sub_args.next();
                } else {
                    return Err(format!("unknown debug flag '{flag}'"));
                }
            }

            if let Some(path) = reload_path {
                let changed_artifacts = compile_file(Path::new(&path), &CompileOptions::default())
                    .map_err(render_diags)?;

                if let Ok(base_native) = &base_image {
                    let changed_image = build_native_image(&changed_artifacts.program)?;
                    let patch = plan_native_patch(base_native, &changed_image);
                    for name in patch.patched_functions {
                        let abi_hash = changed_image.abi_hashes.get(&name).ok_or_else(|| {
                            format!("missing ABI hash for patched symbol '{name}'")
                        })?;
                        let code_ptr = changed_image.code_ptrs.get(&name).ok_or_else(|| {
                            format!("missing code pointer for patched symbol '{name}'")
                        })?;
                        let rev = runtime
                            .fn_table
                            .patch_checked(&name, *abi_hash, *code_ptr)?;
                        println!("patched {} to revision {}", rev.name, rev.revision);
                    }
                    if !patch.rejected_functions.is_empty() {
                        println!("rejected patch targets (ABI mismatch/new or removed):");
                        for name in patch.rejected_functions {
                            println!("  {name}");
                        }
                    }
                    println!(
                        "restart required: {}",
                        if patch.restart_required { "yes" } else { "no" }
                    );
                } else {
                    let mut session = DebugSession::from_program(&artifacts.program);
                    let delta = session.reload(&changed_artifacts.program);
                    for name in delta.recompiled_functions {
                        let rev = runtime.fn_table.patch_checked(&name, 0, 0)?;
                        println!("patched {} to revision {}", rev.name, rev.revision);
                    }
                    println!(
                        "restart required: {}",
                        if delta.restart_required { "yes" } else { "no" }
                    );
                }
                println!(
                    "allocations preserved across patch: {}",
                    runtime.arena.allocation_count()
                );
            }
            Ok(0)
        }
        "build" => {
            let file = sub_args
                .next()
                .ok_or_else(|| "missing file path".to_string())?;
            let mut kind = LibraryKind::Dll;
            let mut out: Option<PathBuf> = None;
            while let Some(flag) = sub_args.next() {
                match flag.as_str() {
                    "--dll" => kind = LibraryKind::Dll,
                    "--lib" | "--staticlib" => kind = LibraryKind::StaticLib,
                    "--out" => {
                        let path = sub_args
                            .next()
                            .ok_or_else(|| "missing value after --out".to_string())?;
                        out = Some(PathBuf::from(path));
                    }
                    _ => return Err(format!("unknown build flag '{flag}'")),
                }
            }
            let artifacts =
                compile_file(Path::new(&file), &CompileOptions::default()).map_err(render_diags)?;
            let output = out.unwrap_or_else(|| default_build_output(&file, kind));
            build_library(&artifacts.program, &output, kind)?;
            println!("built library: {}", output.display());
            Ok(0)
        }
        "--help" | "-h" | "help" => {
            print_usage();
            Ok(0)
        }
        _ => {
            print_usage();
            Ok(1)
        }
    }
}
