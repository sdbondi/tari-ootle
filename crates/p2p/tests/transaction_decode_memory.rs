//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Heap cost of decoding a peer-supplied transaction.
//!
//! Kept in a test binary of its own because it installs a counting global allocator, and any other
//! test running alongside it would be counted too.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
};

use tari_crypto::ristretto::RistrettoSecretKey;
use tari_ootle_p2p::proto;
use tari_ootle_transaction::{Epoch, Instruction, Transaction};

/// `ConsensusConstants::max_transaction_size_bytes`: the most bytes a transaction may encode to and
/// still be relayed.
const MAX_TRANSACTION_SIZE_BYTES: usize = 1_310_720;

struct Counting;

static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = OUTSTANDING.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        OUTSTANDING.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                let grown = new_size - layout.size();
                let now = OUTSTANDING.fetch_add(grown, Ordering::Relaxed) + grown;
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                OUTSTANDING.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The wire form of a sealed transaction carrying `n` copies of the cheapest instruction to encode.
fn wire_with_drop_instructions(n: usize) -> proto::transaction::Transaction {
    let transaction = Transaction::builder_localnet(Epoch(1))
        .with_instructions((0..n).map(|_| Instruction::DropAllProofsInWorkspace))
        .build_and_seal(&RistrettoSecretKey::from(1u64));
    proto::transaction::Transaction::from(&transaction)
}

/// The most heap decoding `wire` takes at any point, over what was allocated before it started.
fn peak_decode_heap(wire: proto::transaction::Transaction) -> usize {
    let baseline = OUTSTANDING.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let decoded = Transaction::try_from(wire);
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    drop(black_box(decoded));
    peak
}

/// A transaction that fits the byte cap must not cost many times its size to decode, whatever it
/// carries: every validator decodes every relayed transaction before it can judge it.
#[test]
fn decoding_a_max_size_transaction_costs_a_bounded_multiple_of_its_size() {
    let empty = wire_with_drop_instructions(0).bor_encoded.len();
    let per_instruction = (wire_with_drop_instructions(1000).bor_encoded.len() - empty).div_ceil(1000);
    let wire = wire_with_drop_instructions((MAX_TRANSACTION_SIZE_BYTES - empty) / per_instruction);
    let wire_len = wire.bor_encoded.len();
    assert!(wire_len <= MAX_TRANSACTION_SIZE_BYTES);

    let peak = peak_decode_heap(wire);

    assert!(
        peak < 4 * wire_len,
        "decoding {wire_len} bytes peaked at {peak} bytes of heap ({:.1}x)",
        peak as f64 / wire_len as f64
    );
}
