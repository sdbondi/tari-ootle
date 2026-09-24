//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! A transaction declares each input as a read or a write. A shard group that does not hold an input
//! locks it from that declaration alone, without executing, so the declaration has to be an upper
//! bound on access rather than a hint: writing to a read-declared input aborts here.

use ootle_byte_type::ToByteType;
use tari_engine::runtime::{ActionIdent, NativeAction, RuntimeError};
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{InputDeclaration, substate_type::SubstateType};
use tari_ootle_transaction::{Epoch, Transaction, args};
use tari_template_lib::types::{
    AccessRule,
    Amount,
    ComponentAddress,
    OwnerRule,
    access_rules::{ComponentAccessRules, ResourceAccessRules, ResourceAuthAction},
    constants::XTR_FAUCET_CLAIM_RESOURCE_ADDRESS,
};
use tari_template_test_tooling::{TemplateTest, support::assert_error::assert_reject_reason, xtr_faucet_component};

const CRATE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"));

fn setup() -> (TemplateTest, ComponentAddress) {
    let mut test = TemplateTest::new(CRATE_PATH, vec!["tests/templates/state"]);
    let component: ComponentAddress = test.call_function("State", "new", args![], vec![]);
    (test, component)
}

#[test]
fn a_read_declared_input_may_be_read() {
    let (mut test, component) = setup();

    let value: u32 = test.call_method(component, "get", args![], vec![]);
    assert_eq!(value, 0);

    let result = test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .add_input(InputDeclaration::read(component))
            .call_method(component, "get", args![])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    assert!(result.finalize.is_accept());
}

#[test]
fn writing_to_a_read_declared_input_aborts() {
    let (mut test, component) = setup();

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .add_input(InputDeclaration::read(component))
            .call_method(component, "set", args![42u32])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    assert_reject_reason(&reason, RuntimeError::WriteToReadDeclaredInput {
        id: SubstateId::Component(component),
    });

    // The abort is what stops the write, so the component still holds its original value.
    let value: u32 = test.call_method(component, "get", args![], vec![]);
    assert_eq!(value, 0);
}

#[test]
fn a_write_declared_input_may_be_written() {
    let (mut test, component) = setup();

    test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .add_input(InputDeclaration::write(component))
            .call_method(component, "set", args![42u32])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    let value: u32 = test.call_method(component, "get", args![], vec![]);
    assert_eq!(value, 42);
}

#[test]
fn a_substate_declared_both_ways_is_a_write() {
    let (mut test, component) = setup();

    // `add_input` widens rather than letting declaration order decide, in either order.
    for (first, second) in [
        (InputDeclaration::read(component), InputDeclaration::write(component)),
        (InputDeclaration::write(component), InputDeclaration::read(component)),
    ] {
        let transaction = Transaction::builder_localnet(Epoch(1))
            .add_input(first)
            .add_input(second)
            .call_method(component, "set", args![7u32])
            .build_and_seal(test.secret_key());

        assert_eq!(transaction.inputs().len(), 1);
        test.execute_expect_success(transaction, vec![]);
    }

    let value: u32 = test.call_method(component, "get", args![], vec![]);
    assert_eq!(value, 7);
}

#[test]
fn minting_and_burning_leave_a_resource_without_supply_tracking_as_a_read() {
    let mut test = TemplateTest::new(CRATE_PATH, Vec::<&str>::new());
    let (owner_proof, public_key, secret_key) = test.create_owner_proof();

    // The faucet claim mints a receipt on the claim resource and burns it straight away. The claim resource does
    // not track supply, so neither operation alters it.
    test.execute_expect_success(
        test.transaction()
            .add_input(InputDeclaration::read(XTR_FAUCET_CLAIM_RESOURCE_ADDRESS))
            .create_account(public_key.to_byte_type())
            .put_last_instruction_output_on_workspace("account")
            .call_method(xtr_faucet_component(), "take", args![Workspace("account")])
            .build_and_seal(&secret_key),
        vec![owner_proof],
    );
}

#[test]
fn burning_from_a_read_declared_resource_that_tracks_supply_aborts() {
    let mut test = TemplateTest::new(CRATE_PATH, vec!["tests/templates/faucet"]);
    test.execute_expect_success(
        test.transaction()
            .call_function(test.get_template_address("TestFaucet"), "mint", args![Amount::new(
                1000
            )])
            .build_and_seal(test.secret_key()),
        vec![],
    );
    let faucet = test.get_previous_output_address(SubstateType::Component);
    let resource = test.get_previous_output_address(SubstateType::Resource);

    let reason = test.execute_expect_failure(
        test.transaction()
            .add_input(InputDeclaration::read(resource.clone()))
            .call_method(faucet.as_component_address().unwrap(), "burn_coins", args![
                Amount::new(10)
            ])
            .build_and_seal(test.secret_key()),
        vec![],
    );

    assert_reject_reason(&reason, RuntimeError::WriteToReadDeclaredInput { id: resource });
}

#[test]
fn an_unauthorized_access_rule_update_on_a_read_declared_resource_is_denied_access() {
    let mut test = TemplateTest::new(CRATE_PATH, vec!["tests/templates/access_rules"]);
    let (owner_proof, _, owner_key) = test.create_owner_proof();

    // The default resource rules lock the mint updater, so not even the owner may change the mint rule.
    let result = test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .call_function(
                test.get_template_address("AccessRulesTest"),
                "with_configured_rules",
                args![
                    OwnerRule::OwnedBySigner,
                    ComponentAccessRules::new().default(AccessRule::AllowAll),
                    ResourceAccessRules::new(),
                    AccessRule::DenyAll,
                ],
            )
            .build_and_seal(&owner_key),
        vec![owner_proof.clone()],
    );
    let diff = result.finalize.result.any_accept().unwrap();
    let component = diff.up_iter().find_map(|(id, _)| id.as_component_address()).unwrap();
    let resources = diff
        .up_iter()
        .filter(|(id, _)| id.is_resource())
        .map(|(id, _)| InputDeclaration::read(id.clone()))
        .collect::<Vec<_>>();

    let reason = test.execute_expect_failure(
        Transaction::builder_localnet(Epoch(1))
            .with_inputs(resources)
            .call_method(component, "update_tokens_access_rule", args![
                ResourceAuthAction::Mint,
                AccessRule::AllowAll
            ])
            .build_and_seal(&owner_key),
        vec![owner_proof],
    );

    assert_reject_reason(&reason, RuntimeError::AccessDenied {
        action_ident: ActionIdent::Native(NativeAction::UpdateResourceAccessRule(ResourceAuthAction::Mint)),
    });
}
