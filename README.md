# Xlang (Prototype)

Xlang is a Python-like, statically typed, memory-safe systems language prototype.

# Plan for v2

We are intending to make a custom compiler that surpasses most modern day compilers in pure speed and memory. 

## Current capabilities

- Indentation-based syntax with explicit types.
- Friendlier syntax options:
  - `fn`, `def`, or `function` for declarations
  - optional `-> return_type` (defaults/infers to `i64`)
  - optional type on `let` (`let x = ...`)
  - first assignment declares variable (`x = ...`)
  - `if ...` + `is ...:` arms for switch-style branching (`i64`, `bool`, `str`)
  - multiple patterns per arm: `is 1, 2, 3:`
  - richer `is` patterns: `is < 10:`, `is >= 5:`, `is 10..20:`, `is starts_with "ab":`, `is contains "x":`, `is ends_with "z":`
  - default branches via `else:` or `is default:` / `is none:` / `is null:`
  - standard `if/elif/else`
  - `while`, `for i in a..b`, `break`, `continue`
  - threaded execution suffix:
    - `fn_call(...) -> thread()`
    - `fn_call(...) -> thread(10)`
    - `fn_call(...) -> thread(4).wait()`
    - `while cond: -> thread(n)[.wait()]`
  - stepped loops: `for i in a..b step s`
  - `pass` no-op statement
  - expressions: `== != < <= > >= and or not` and unary `-x`
  - method-call sugar: `value.method(a, b)` rewrites to `method(value, a, b)`
  - `%` modulo operator
  - string concatenation with `+` (`str + str`)
  - inline comments with both `#` and `//`
  - block comments with `~ { ... } ~` (can span multiple lines)
  - compound assignment: `+= -= *= /=`
  - variadic `print(...)` / `println(...)` (newline)
  - variadic `write(...)` (no newline)
  - Python-like `input()` / `input(">> ")` returning `str`
  - process/CLI builtins: `argc()`, `argv(i)`, `sleep_ms(ms)`
  - file modules with `import "relative_module.x"`
  - top-level C imports with `extern ... from "library.dll"`
  - low-level pointer helpers for FFI: `ptr_can_read`, `ptr_can_write`, `ptr_read8`, `ptr_write8`, `ptr_read64`, `ptr_write64`
    - invalid reads return `0`; invalid writes return `-1` (non-crashing behavior)
- Type checker + basic move-only borrow checks for non-copy values (`str`).
- Multithreaded compile pipeline for type checking, borrow checking, and optimization passes.
- Constant-folding optimization pass (parallelized across functions).
- Cranelift JIT native backend for `i64`/`bool`/`str` execution (`x` run path is native-only).
- Selective stdlib reachability analysis (tree-shaking model).
- Debug session function-level patch planning with ABI compatibility checks and allocation preservation simulation.
- Runtime patch table now stores native code pointers and ABI hashes for checked function swaps.
- Optimizer now does constant folding plus small return-only function inlining.
- CLI commands:
  - `x examples/hello.x` (Python-like direct script run)
  - `x check examples/hello.x`
  - `x check examples/hello.x --timings`
  - `x run examples/hello.x`
  - `x run examples/hello.x --timings`
  - `x debug examples/hello.x --reload examples/hello_patch.x`
  - `x debug examples/hello.x --reload examples/hello_patch_breaking.x`
  - `x build examples/libmath.x --dll --out build/libmath.dll`
  - `x build examples/libmath.x --lib --out build/libmath.lib`
  - `x examples/modules_main.x`
  - `x examples/for_range.x`
  - `x examples/ops.x`
  - `x examples/if_is_advanced.x`
  - `x examples/qol.x`
  - `x examples/step_loop.x`
  - `x examples/strings_and_mod.x`
  - `x examples/string_qol.x`
  - `x examples/comptime_inline.x`
  - `x examples/tui_hub.x`
  - `x examples/input_echo.x`
  - `x examples/cli_args.x -- alpha beta`
  - `x examples/thread_demo.x`
  - `x examples/pointer_guard.x`
  - `x bootstrap/stage2/compiler_subset.x`

## Build/test

```bash
cargo test
cargo run -p xlangc --bin x -- examples/hello.x
cargo run -p x-docmaker -- docs/doc_manifest.xdocs docs/generated
pwsh tests/bench.ps1
```

## Documentation generator

Use the third-party-backed docs generator (`rayon` parallel renderer):

```bash
cargo run -p x-docmaker -- docs/doc_manifest.xdocs docs/generated
```

Generated pages:
- `docs/generated/INDEX.md`
- `docs/generated/REFERENCE.md`
- `docs/generated/BUILTINS.md`
- `docs/generated/CLI.md`

## Code Layout

- `compiler/src/codegen.rs`: interpreter execution core (control flow + expression evaluation)
- `compiler/src/codegen/ffi.rs`: dynamic C-extern loading/invocation layer
- `compiler/src/codegen/builtins_runtime.rs`: runtime builtin implementations
- `tools/cli/src/main.rs`: CLI orchestration/dispatch
- `tools/cli/src/commands.rs`: `check`/`run` command handlers + output path helpers
- `tools/cli/src/output.rs`: diagnostics, usage text, and timing renderers

## Friendly syntax example

```x
function add(a: i64, b: i64):
    return a + b

def main():
    result = add(20, 22)
    print("Result: ")
    print(result)
    println()
    return result
```

## Switch-style `if is` example

```x
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
```

## Module import example

```x
# examples/modules_main.x
import "modules_math.x"

fn main() -> i64:
    return mul(6, 7)
```

## For-range example

```x
fn main() -> i64:
    let sum = 0
    for i in 1..10:
        if i
            is 3:
                continue
            is 8:
                break
        sum = sum + i
    return sum
```

## C import example

```x
extern fn _abs64(v: i64) -> i64 from "msvcrt.dll"

fn main() -> i64:
    return _abs64(0 - 42)
```
