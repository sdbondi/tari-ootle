//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine::runtime::{ActionIdent, RuntimeError};
use tari_engine_types::commit_result::ExecutionFailureCode;
use tari_ootle_transaction::{Epoch, Transaction, args, args::WorkspaceOffsetId};
use tari_template_lib::{
    models::ProofId,
    types::{
        AccessRule,
        ComponentAddress,
        OwnerRule,
        access_rules::{ComponentAccessRules, ResourceAccessRules},
        constants::TARI_TOKEN,
        rule,
    },
};
use tari_template_test_tooling::{
    TemplateTest,
    support::assert_error::{assert_access_denied_for_action, assert_reject_reason_with_code},
};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

#[test]
fn it_opens_and_closes_an_authorization_window() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (account, owner_proof, owner_key) = test.create_funded_account();

    test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "create_proof_for_resource", args![TARI_TOKEN])
            .put_last_instruction_output_on_workspace("proof")
            .with_authorized_proof("proof", |builder| builder)
            .drop_all_proofs_in_workspace()
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );
}

#[test]
fn it_rejects_dropping_an_authorization_that_is_not_open() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (account, owner_proof, owner_key) = test.create_funded_account();

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "create_proof_for_resource", args![TARI_TOKEN])
            .put_last_instruction_output_on_workspace("proof")
            .with_authorized_proof("proof", |builder| builder)
            .drop_proof_authorization("proof")
            .drop_all_proofs_in_workspace()
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_reject_reason_with_code(
        reason,
        RuntimeError::ProofNotAuthorized {
            proof_id: ProofId::from(0),
        },
        ExecutionFailureCode::InvalidArgument,
    );
}

#[test]
fn it_rejects_authorizing_a_workspace_item_that_is_not_a_proof() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (account, owner_proof, owner_key) = test.create_funded_account();

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "withdraw", args![TARI_TOKEN, 10])
            .put_last_instruction_output_on_workspace("bucket")
            .authorize_proof("bucket")
            .call_method(account, "deposit", args![Workspace("bucket")])
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_reject_reason_with_code(
        reason,
        RuntimeError::WorkspaceItemNotAProof {
            key: WorkspaceOffsetId::new(0),
        },
        ExecutionFailureCode::InvalidArgument,
    );
}

#[test]
fn it_rejects_dropping_the_authorization_of_a_workspace_item_that_is_not_a_proof() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (account, owner_proof, owner_key) = test.create_funded_account();

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "withdraw", args![TARI_TOKEN, 10])
            .put_last_instruction_output_on_workspace("bucket")
            .drop_proof_authorization("bucket")
            .call_method(account, "deposit", args![Workspace("bucket")])
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_reject_reason_with_code(
        reason,
        RuntimeError::WorkspaceItemNotAProof {
            key: WorkspaceOffsetId::new(0),
        },
        ExecutionFailureCode::InvalidArgument,
    );
}

#[test]
fn it_rejects_authorizing_a_dropped_proof() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (account, owner_proof, owner_key) = test.create_funded_account();

    // Dropping the workspace's proofs leaves the item at "proof" naming a proof that no longer exists.
    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "create_proof_for_resource", args![TARI_TOKEN])
            .put_last_instruction_output_on_workspace("proof")
            .drop_all_proofs_in_workspace()
            .authorize_proof("proof")
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_reject_reason_with_code(
        reason,
        RuntimeError::ProofNotInScope {
            proof_id: ProofId::from(0),
        },
        ExecutionFailureCode::InvalidArgument,
    );
}

#[test]
fn it_revokes_method_access_when_the_authorization_is_dropped() {
    let mut test = TemplateTest::new(CRATE_PATH, ["tests/templates/access_rules"]);
    let (account, owner_proof, owner_key) = test.create_funded_account();
    let access_rules_template = test.get_template_address("AccessRulesTest");

    test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .call_function(access_rules_template, "with_configured_rules", args![
                OwnerRule::None,
                ComponentAccessRules::new()
                    .add_method_rule("set_value", rule!(resource(TARI_TOKEN)))
                    .default(AccessRule::AllowAll),
                ResourceAccessRules::new(),
                AccessRule::DenyAll,
            ])
            .build_and_seal(&owner_key),
        vec![owner_proof.clone()],
    );
    let component_address = test.return_values()[0].decoded::<ComponentAddress>().unwrap();

    test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "create_proof_for_resource", args![TARI_TOKEN])
            .put_last_instruction_output_on_workspace("proof")
            .with_authorized_proof("proof", |builder| {
                builder.call_method(component_address, "set_value", args![1u32])
            })
            .drop_all_proofs_in_workspace()
            .build_and_seal(&owner_key),
        vec![owner_proof.clone()],
    );

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .call_method(account, "create_proof_for_resource", args![TARI_TOKEN])
            .put_last_instruction_output_on_workspace("proof")
            .with_authorized_proof("proof", |builder| builder)
            .call_method(component_address, "set_value", args![2u32])
            .drop_all_proofs_in_workspace()
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_access_denied_for_action(reason, ActionIdent::ComponentCallMethod {
        component_address,
        method: "set_value".to_string(),
    });
}
