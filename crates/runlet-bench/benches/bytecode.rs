//! Microbenchmark: source `Module::declare` (parse + compile) vs `Module::load` (deserialize
//! precompiled bytecode) — the exact per-call delta `runlet-core`'s bytecode cache trades on
//! (mirrors `engine.rs`: `obtain_declared` compiles on a miss, `load_bytecode` loads on a hit).
//!
//! Both arms create a **fresh `Context` per iteration** and run declare-or-load → `eval` →
//! `finish`, matching the engine's real per-request model (a fresh context every call). Context
//! creation is therefore a constant offset present in *both* arms, so the meaningful figure is
//! the **difference** between the `compile` and `load` benches at a given script size: that
//! difference is what a cache hit saves per invocation. Larger handlers parse for longer, so
//! the `large` pair shows the saving growing with source size.
//!
//! Run: `cargo bench -p runlet-bench`

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rquickjs::module::Declared;
use rquickjs::{Context, Ctx, Module, Runtime, WriteOptions};

/// A small but realistic ES-module handler.
const SMALL: &str = "export default function handler(ctx) { return json(ctx.n + 1); }";

/// Builds a larger handler (a chain of helper functions + the handler) to show compile cost
/// scaling with source size. `repeat` controls how many helpers are generated.
fn large_source(repeat: usize) -> String {
    let mut src = String::new();
    for i in 0..repeat {
        src.push_str(&format!(
            "function step{i}(x) {{ return (x * {i} + {i}) % 1000007; }}\n"
        ));
    }
    src.push_str("export default function handler(ctx) {\n  let acc = ctx.n;\n");
    for i in 0..repeat {
        src.push_str(&format!("  acc = step{i}(acc);\n"));
    }
    src.push_str("  return json(acc);\n}\n");
    src
}

/// Declares (parse + compile) the source as a module and evaluates it to completion.
fn compile_and_eval(qctx: &Ctx<'_>, source: &str) {
    let declared = Module::declare(qctx.clone(), "handler", source)
        .unwrap_or_else(|_err| unreachable!("benchmark source must compile"));
    eval_declared(declared);
}

/// Loads precompiled bytecode as a module and evaluates it to completion.
fn load_and_eval(qctx: &Ctx<'_>, bytecode: &[u8]) {
    // SAFETY: `bytecode` came from `write_bytecode` on a module compiled in this same process,
    // so it is valid QuickJS bytecode for this build — the same invariant runlet-core relies on.
    let declared = unsafe { Module::load(qctx.clone(), bytecode) }
        .unwrap_or_else(|_err| unreachable!("benchmark bytecode must load"));
    eval_declared(declared);
}

/// Evaluates a declared module to completion (pumps the job queue via `finish`).
fn eval_declared(declared: Module<'_, Declared>) {
    let (module, promise) = declared
        .eval()
        .unwrap_or_else(|_err| unreachable!("module must evaluate"));
    promise
        .finish::<()>()
        .unwrap_or_else(|_err| unreachable!("module must settle"));
    black_box(module);
}

/// Compiles `source` once and returns its serialized bytecode (native endianness).
fn write_bytecode(runtime: &Runtime, source: &str) -> Vec<u8> {
    let ctx = Context::full(runtime).unwrap_or_else(|_err| unreachable!());
    ctx.with(|qctx| {
        Module::declare(qctx.clone(), "handler", source)
            .unwrap_or_else(|_err| unreachable!())
            .write(WriteOptions::default())
            .unwrap_or_else(|_err| unreachable!())
    })
}

/// Sweeps compile-vs-load across source sizes so the parse/compile saving can be read off as a
/// function of source bytes. Each `repeat` adds one helper fn + one call (~75 bytes of *code*,
/// not data). Benchmark ids are the actual source byte length, so the crossover is read directly
/// from the report. `SMALL` is included as the ~60-byte floor.
fn bench_bytecode(crit: &mut Criterion) {
    let runtime = Runtime::new().unwrap_or_else(|_err| unreachable!());

    // repeat counts chosen to land near ~60 B, 512 B, 1/2/4/8/16/32/64 KB.
    let repeats = [0_usize, 7, 13, 26, 52, 105, 210, 420, 840];
    let mut sources: Vec<String> = repeats.iter().map(|&r| large_source(r)).collect();
    sources.insert(0, SMALL.to_owned());

    let mut group = crit.benchmark_group("module_setup");
    // Trimmed sampling — we want the size trend, not 0.1 % precision per point.
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));

    for source in &sources {
        let bytes_len = source.len() as u64;
        let bytecode = write_bytecode(&runtime, source);
        group.bench_with_input(
            BenchmarkId::new("compile", bytes_len),
            source.as_str(),
            |bencher, src| {
                bencher.iter(|| {
                    let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());
                    ctx.with(|qctx| compile_and_eval(&qctx, black_box(src)));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("load", bytes_len),
            &bytecode,
            |bencher, bc| {
                bencher.iter(|| {
                    let ctx = Context::full(&runtime).unwrap_or_else(|_err| unreachable!());
                    ctx.with(|qctx| load_and_eval(&qctx, black_box(bc)));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_bytecode);
criterion_main!(benches);
