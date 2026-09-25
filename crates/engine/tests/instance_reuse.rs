//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! A template's calls within one transaction share one WASM instance, yet every call starts from the
//! state of a fresh instantiation: nothing one call leaves in the guest reaches the next.

use tari_ootle_transaction::args;
use tari_template_lib::types::TemplateAddress;
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const TEMPLATE_NAME: &str = "InstanceReuse";
const CRATE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/templates");

fn setup() -> (TemplateTest, TemplateAddress) {
    let test = TemplateTest::new(CRATE_PATH, ["instance_reuse"]);
    let template = test.get_template_address(TEMPLATE_NAME);
    (test, template)
}

#[test]
fn a_guest_static_does_not_outlive_its_call() {
    let (mut test, template) = setup();

    let result = test.execute_expect_success(
        test.transaction()
            .call_function(template, "count", args![])
            .call_function(template, "count", args![])
            .call_function(template, "count", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let counts: Vec<u32> = result
        .finalize
        .execution_results
        .iter()
        .map(|r| r.decode().unwrap())
        .collect();
    assert_eq!(counts, [1, 1, 1]);
}

#[test]
fn memory_grown_by_one_call_is_not_grown_for_the_next() {
    let (mut test, template) = setup();

    let result = test.execute_expect_success(
        test.transaction()
            .call_function(template, "grow_memory", args![])
            .call_function(template, "grow_memory", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let results = &result.finalize.execution_results;
    assert_eq!(results[0].decode::<u32>().unwrap(), results[1].decode::<u32>().unwrap());
}

#[test]
fn memory_written_by_one_call_is_zero_for_the_next() {
    let (mut test, template) = setup();

    let result = test.execute_expect_success(
        test.transaction()
            .call_function(template, "scribble_last_byte", args![])
            .call_function(template, "read_last_byte", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    assert_eq!(result.finalize.execution_results[1].decode::<u8>().unwrap(), 0);
}

#[test]
fn a_reentrant_call_starts_fresh() {
    let (mut test, template) = setup();

    let result = test.execute_expect_success(
        test.transaction()
            .call_function(template, "count", args![])
            .call_function(template, "count_reentrant", args![template])
            .call_function(template, "count", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let results = &result.finalize.execution_results;
    assert_eq!(results[0].decode::<u32>().unwrap(), 1);
    assert_eq!(results[1].decode::<(u32, u32)>().unwrap(), (1, 1));
    assert_eq!(results[2].decode::<u32>().unwrap(), 1);
}

#[test]
fn a_trap_rejects_the_transaction() {
    let (mut test, template) = setup();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_function(template, "count", args![])
            .call_function(template, "count_then_trap", args![])
            .call_function(template, "count", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    assert_reject_reason(&reason, "unreachable");
}

#[test]
fn a_panic_message_does_not_outlive_its_call() {
    let (mut test, template) = setup();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_function(template, "plant_panic_message", args![])
            .call_function(template, "count_then_trap", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    assert!(
        !reason.to_string().contains("planted by an earlier call"),
        "a trap was reported with a panic message recorded by an earlier call: {reason}"
    );
    assert_reject_reason(&reason, "unreachable");
}
