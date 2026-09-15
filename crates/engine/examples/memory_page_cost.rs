//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Measures what one page of WASM linear memory costs a validator, so `memory.grow` and a module's
//! declared initial memory can be priced from the same measured rate.
//!
//!     cargo run -p tari_engine --example memory_page_cost --release
//!
//! Method matches `metering_recost`: time a bench at two round counts and take the slope, which
//! cancels fixed per-call overhead. `bench_cheap8` minus `bench_noop`, over 8, gives the cost of one
//! cheap op — the "1 point" unit every other operator is priced against — so a page's cost lands in
//! the same currency as the rest of the table.
//!
//! Two figures are reported because they answer different questions:
//!
//! - **grow, untouched** is what the operator itself costs: the host-side mapping. This is what `memory.grow` should be
//!   priced at, since a guest that never touches the page never makes the validator materialise it.
//! - **grow + first touch** adds the page fault. A guest writing across a page pays for that through its own metered
//!   stores; one writing a single byte per page does not. `max_memory_pages` is what bounds that exposure.
//!
//! The engine's page cap (`limits::WASM_LIMITS.max_memory_pages`) bounds a real module far below the
//! round counts here. Raise it for the duration of a run.

use std::time::Instant;

use tari_ootle_transaction::args;
use tari_template_test_tooling::TemplateTest;

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const TEMPLATE: &str = "tests/templates/metering_bench";

/// Must match `INNER` in the template: the integer and float grinders do `INNER` measured ops per
/// round, while the grow benches do one page per round.
const INNER: f64 = 64.0;

/// Round counts for the two-point slope on the cheap-op reference.
const R1: u64 = 35_000;
const R2: u64 = 70_000;

/// Pages grown for the two-point slope. The upper figure is ~1 GiB of address space, which is
/// mapping, not memory, while no page is touched.
const P1: u64 = 2_000;
const P2: u64 = 16_000;

const TRIALS: usize = 9;

fn main() {
    eprintln!("Compiling metering_bench template and warming up...");
    let mut test = TemplateTest::new(CRATE_PATH, [TEMPLATE]);

    let time = |test: &mut TemplateTest, func: &str, rounds: u64| -> f64 {
        let _: u64 = test.call_function("MeteringBench", func, args![rounds], vec![]);
        let mut best = f64::MAX;
        for _ in 0..TRIALS {
            let start = Instant::now();
            let _: u64 = test.call_function("MeteringBench", func, args![rounds], vec![]);
            best = best.min(start.elapsed().as_nanos() as f64);
        }
        best
    };

    // ns per cheap op — the unit the metering table is denominated in.
    let per_round = |test: &mut TemplateTest, func: &str, r1: u64, r2: u64| -> f64 {
        (time(test, func, r2) - time(test, func, r1)) / (r2 - r1) as f64
    };
    let noop = per_round(&mut test, "bench_noop", R1, R2) / INNER;
    let cheap_op_ns = (per_round(&mut test, "bench_cheap8", R1, R2) / INNER - noop) / 8.0;

    let grow_ns = per_round(&mut test, "bench_memory_grow", P1, P2);
    let touch_ns = per_round(&mut test, "bench_memory_grow_touch", P1, P2);
    // Eight pages per call, so the slope is per call; divide by 8 for the per-page figure.
    let grow8_ns = per_round(&mut test, "bench_memory_grow8", P1 / 8, P2 / 8) / 8.0;

    println!("\n1 cheap op ≈ {cheap_op_ns:.3} ns (the 1-point unit)\n");
    println!("{:<26} {:>12} {:>14}", "", "ns/page", "points/page");
    for (label, ns) in [
        ("grow 1 page/call", grow_ns),
        ("grow 8 pages/call", grow8_ns),
        ("grow + first touch", touch_ns),
    ] {
        println!("{:<26} {:>12.1} {:>14.0}", label, ns, ns / cheap_op_ns);
    }
    println!(
        "\nfirst touch alone: {:.1} ns/page ({:.0} points) — covered by the guest's own stores only if it writes \
         across the page",
        touch_ns - grow_ns,
        (touch_ns - grow_ns) / cheap_op_ns,
    );
    println!("\nmemory.grow is priced flat per call by `metering::MEMORY_GROW_COST`, not per page.");
}
