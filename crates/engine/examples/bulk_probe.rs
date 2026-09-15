//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Splits a workload's bulk-memory charge into its two terms so the right fix can be chosen.
//!
//!     cargo run -p tari_engine --example bulk_probe --release
//!
//! `bulk cost = BULK_OPERATOR_COST x (executed bulk ops) + POINTS_PER_MEMORY_BYTE x (bytes copied)`.
//! Run once as shipped, once with `POINTS_PER_MEMORY_BYTE = 0` and once with
//! `BULK_OPERATOR_COST = 2`; the deltas give the op count and the byte volume separately.

use ootle_byte_type::ToByteType;
use tari_ootle_transaction::args;
use tari_template_test_tooling::{TemplateTest, xtr_faucet_component};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

fn main() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.disable_fees();

    let (owner_proof, public_key, secret_key) = test.create_owner_proof();
    let transaction = test
        .transaction()
        .create_account(public_key.to_byte_type())
        .put_last_instruction_output_on_workspace("account")
        .call_method(xtr_faucet_component(), "take", args![Workspace("account")])
        .build_and_seal(&secret_key);
    let result = test.execute_expect_success(transaction, vec![owner_proof]);

    println!(
        "faucet claim: wasm={} native={}",
        result.wasm_execution_points, result.native_execution_points
    );
}
