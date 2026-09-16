//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Measures verifying a transaction's authorization signatures one at a time against verifying them
//! as one batch, as a function of the number of signatures.
//!
//! Every authorization signature over a transaction signs the same message, so
//! `TransactionSignature::verify_all_against_message` folds the set into a single multiscalar
//! multiplication. This is what sizes `MAX_SIGNATURES_PER_TRANSACTION`: the cap exists because every
//! node verifies these before any fee is charged, so the cost per signature is what a ceiling has to
//! be set against. It also fixes `MIN_BATCH_SIZE`, the count below which the batch's fixed cost is
//! not yet repaid.
//!
//! Signatures are made over a synthetic message rather than a built transaction: deriving the
//! message hashes the whole body and is shared by both paths, so including it would measure the same
//! work twice and dilute what is being compared.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ootle_byte_type::ToByteType;
use tari_crypto::{
    keys::{PublicKey, SecretKey},
    ristretto::{RistrettoPublicKey, RistrettoSchnorr, RistrettoSecretKey},
};
use tari_ootle_transaction::TransactionSignature;

/// `n` valid signatures over `message`, each from a distinct key — the shape a multi-input stealth
/// spend or a coinjoin produces, where every input contributes its own one-time key.
fn signatures(n: usize, message: &[u8; 64]) -> Vec<TransactionSignature> {
    let mut rng = rand::rng();
    (0..n)
        .map(|_| {
            let secret = RistrettoSecretKey::random(&mut rng);
            let public_key = RistrettoPublicKey::from_secret_key(&secret);
            let signature = RistrettoSchnorr::sign(&secret, message, &mut rng).expect("sign is infallible");
            TransactionSignature::new(public_key.to_byte_type(), signature.to_byte_type())
        })
        .collect()
}

/// The pre-batch verification path: one double-base multiplication per signature, against the same
/// already-derived message.
fn verify_individually(signatures: &[TransactionSignature], message: [u8; 64]) -> bool {
    signatures.iter().all(|sig| sig.verify_message(message))
}

fn bench(c: &mut Criterion) {
    let message = [7u8; 64];
    let mut g = c.benchmark_group("transaction_signature_verification");

    for n in [1usize, 2, 4, 8, 16, 32, 64, 128, 256, 1024] {
        let sigs = signatures(n, &message);

        g.bench_with_input(BenchmarkId::new("individual", n), &n, |b, _| {
            b.iter(|| assert!(black_box(verify_individually(black_box(&sigs), message))))
        });

        g.bench_with_input(BenchmarkId::new("batch", n), &n, |b, _| {
            b.iter(|| {
                assert!(
                    black_box(TransactionSignature::verify_all_against_message(
                        black_box(&sigs),
                        message
                    ))
                    .is_ok()
                )
            })
        });
    }

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
