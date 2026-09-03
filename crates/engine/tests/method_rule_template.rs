//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine::runtime::{ActionIdent, RuntimeError};
use tari_ootle_transaction::args;
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

/// A `rule!(direct_caller_template(addr))` method access rule allows callers from the template whose address is
/// `addr`, whether they are a component instance of that template or a static function of it.
#[test]
fn direct_caller_template_rule_allows_a_component_caller() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_template_caller",
        "tests/templates/caller_template_callee",
    ]);
    let caller_template = test.get_template_address("TemplateCaller");
    let callee_template = test.get_template_address("TemplateCallee");

    // Create the caller component, then the callee gated on the caller's *template* address. `call_ping`
    // is the control (an unrestricted cross-component call); `call_bar` is allowed because the caller is
    // a component of the caller template.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("caller")
            .call_function(callee_template, "new", args![caller_template])
            .put_last_instruction_output_on_workspace("callee")
            .call_method("caller", "call_ping", args![Workspace("callee")])
            .call_method("caller", "call_bar", args![Workspace("callee")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
}

/// A static function of the gated template is allowed (its template matches the gate).
#[test]
fn direct_caller_template_rule_allows_a_static_function_caller() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_template_caller",
        "tests/templates/caller_template_callee",
    ]);
    let caller_template = test.get_template_address("TemplateCaller");
    let callee_template = test.get_template_address("TemplateCallee");

    // Gate the callee's `bar` on the caller's *template* address. `call_ping_static` is the control; a
    // static function of the caller template is allowed to call `bar`.
    test.execute_expect_success(
        test.transaction()
            .call_function(callee_template, "new", args![caller_template])
            .put_last_instruction_output_on_workspace("callee")
            .call_function(caller_template, "call_ping_static", args![Workspace("callee")])
            .call_function(caller_template, "call_bar_static", args![Workspace("callee")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
}

/// A caller from a different template must be denied.
#[test]
fn direct_caller_template_rule_denies_a_component_of_another_template() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_template_caller",
        "tests/templates/caller_template_callee",
    ]);
    let caller_template = test.get_template_address("TemplateCaller");
    let callee_template = test.get_template_address("TemplateCallee");
    let intruder_template = test.get_template_address("Caller");

    // The callee's `bar` is gated on the `TemplateCaller` template; `Caller` belongs to a different one.
    test.execute_expect_success(
        test.transaction()
            .call_function(callee_template, "new", args![caller_template])
            .call_function(intruder_template, "new", args![])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
    let callee = test
        .read_only_state_store()
        .get_first_component_of(callee_template)
        .unwrap()
        .unwrap();
    let intruder = test
        .read_only_state_store()
        .get_first_component_of(intruder_template)
        .unwrap()
        .unwrap();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(intruder, "call_bar", args![callee])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
    assert_reject_reason(reason, RuntimeError::AccessDenied {
        action_ident: ActionIdent::ComponentCallMethod {
            component_address: callee,
            method: "bar".to_string(),
        },
    });
}
