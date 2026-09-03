//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine::runtime::{ActionIdent, RuntimeError};
use tari_ootle_transaction::args;
use tari_template_lib::types::ComponentAddress;
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

/// A `rule!(caller_component(addr))` method access rule allows the component whose address is `addr` to
/// invoke the method.
///
/// The callee's `bar` method is gated with `rule!(caller_component(caller_address))`. This test invokes
/// `bar` from exactly that caller component and asserts the call succeeds.
#[test]
fn caller_component_rule_allows_the_caller() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_component_callee",
    ]);
    let caller_template = test.get_template_address("Caller");
    let callee_template = test.get_template_address("Callee");

    // Create the caller, then the callee gated on the caller's address. `call_ping` is the control (an
    // unrestricted cross-component call); `call_bar` is allowed because `bar` is restricted to exactly
    // this caller's address.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("caller")
            .call_function(callee_template, "new", args![Workspace("caller")])
            .put_last_instruction_output_on_workspace("callee")
            .call_method("caller", "call_ping", args![Workspace("callee")])
            .call_method("caller", "call_bar", args![Workspace("callee")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
}

/// A component whose address is not the gated address must be denied.
#[test]
fn caller_component_rule_denies_an_unrelated_component() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_component_callee",
    ]);
    let caller_template = test.get_template_address("Caller");
    let callee_template = test.get_template_address("Callee");

    // The callee is gated on the caller created alongside it, which can invoke `bar`.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("allowed")
            .call_function(callee_template, "new", args![Workspace("allowed")])
            .put_last_instruction_output_on_workspace("callee")
            .call_method("allowed", "call_bar", args![Workspace("callee")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
    let callee = test
        .read_only_state_store()
        .get_first_component_of(callee_template)
        .unwrap()
        .unwrap();

    // The intruder is a different instance of the same template, so it must be denied.
    let intruder: ComponentAddress = test.call_function("Caller", "new", args![], vec![test.owner_proof()]);
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

/// A static function has no component identity, so it must never satisfy `rule!(caller_component(addr))`.
#[test]
fn caller_component_rule_denies_a_static_function() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_callee",
        "tests/templates/caller_template_caller",
    ]);
    let caller_template = test.get_template_address("TemplateCaller");
    let callee_template = test.get_template_address("Callee");

    // A concrete component address is used as the gate; the static caller still has no component to match.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("gate")
            .call_function(callee_template, "new", args![Workspace("gate")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
    let callee = test
        .read_only_state_store()
        .get_first_component_of(callee_template)
        .unwrap()
        .unwrap();

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_function(caller_template, "call_bar_static", args![callee])
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

/// A method gated on the component's own address is restricted to that component: a cross-component
/// intruder must be denied.
#[test]
fn caller_component_rule_denies_an_intruder_when_gated_on_own_address() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_component_callee",
    ]);
    let caller_template = test.get_template_address("Caller");
    let callee_template = test.get_template_address("Callee");

    test.execute_expect_success(
        test.transaction()
            .call_function(callee_template, "new_self_gated", args![])
            .call_function(caller_template, "new", args![])
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
        .get_first_component_of(caller_template)
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

/// A top-level transaction signer has no component identity, so a direct `CallMethod` of a method gated
/// on the component's own address must be denied.
#[test]
fn caller_component_rule_denies_a_top_level_signer() {
    let mut test = TemplateTest::new(CRATE_PATH, ["tests/templates/caller_component_callee"]);

    let callee: ComponentAddress = test.call_function("Callee", "new_self_gated", args![], vec![test.owner_proof()]);

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(callee, "bar", args![])
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

/// `OwnerRule::ByAccessRule(rule!(caller_component(addr)))` must be evaluated against the caller, not
/// the callee. The gated caller is the owner and can call a `deny_all` method via the ownership
/// short-circuit; a top-level signer is not the owner and is denied by the method rule.
#[test]
fn caller_component_owner_rule_short_circuits_only_for_the_caller() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_component_callee",
    ]);
    let caller_template = test.get_template_address("Caller");
    let callee_template = test.get_template_address("Callee");

    // The owner component can call `bar` even though its method rule is the default `deny_all`.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("caller")
            .call_function(callee_template, "new_owner_gated", args![Workspace("caller")])
            .put_last_instruction_output_on_workspace("callee")
            .call_method("caller", "call_bar", args![Workspace("callee")])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
    let callee = test
        .read_only_state_store()
        .get_first_component_of(callee_template)
        .unwrap()
        .unwrap();

    // A top-level signer is not the owner, so the ownership check does not short-circuit and the
    // default `deny_all` method rule rejects the call.
    let reason = test.execute_expect_failure(
        test.transaction()
            .call_method(callee, "bar", args![])
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

/// A component owned via `caller_component` can update its own access rules when invoked by that owner:
/// the `SetAccessRules` ownership check evaluates the same caller as the method gate.
#[test]
fn caller_component_owner_can_update_access_rules() {
    let mut test = TemplateTest::new(CRATE_PATH, [
        "tests/templates/caller_component_caller",
        "tests/templates/caller_component_callee",
    ]);
    let caller_template = test.get_template_address("Caller");
    let callee_template = test.get_template_address("Callee");

    // The owner opens `bar` to everyone, after which a top-level signer can call it directly.
    test.execute_expect_success(
        test.transaction()
            .call_function(caller_template, "new", args![])
            .put_last_instruction_output_on_workspace("caller")
            .call_function(callee_template, "new_owner_gated", args![Workspace("caller")])
            .put_last_instruction_output_on_workspace("callee")
            .call_method("caller", "call_open_bar", args![Workspace("callee")])
            .call_method("callee", "bar", args![])
            .build_and_seal(test.secret_key()),
        vec![test.owner_proof()],
    );
}
