//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_transaction::args;
use tari_template_lib_types::{Amount, ComponentAddress, NonFungibleAddress, ResourceAddress, constants::TARI_TOKEN};
use tari_template_test_tooling::{
    TemplateTest,
    crypto::RistrettoSecretKey,
    support::assert_error::assert_reject_reason,
};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");
const NEW_RESOURCE_DEPOSITOR: &str = "tests/new_resource_depositor";

/// Mirrors `MAX_VAULTS` in the account template.
const MAX_ACCOUNT_VAULTS: u32 = 1024;

#[test]
fn it_allows_badge_to_withdraw_from_account() {
    let mut test = TemplateTest::new_builtin_only();

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (user1, user1_proof, user1_secret) = test.create_funded_account();

    // Approve a withdrawal amount for user1
    test.execute_expect_success(
        test.transaction()
            .call_method(owner, "approve", args![user1_proof, TARI_TOKEN, 1000])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof],
    );

    // Withdraw the approved amount
    test.execute_expect_success(
        test.transaction()
            // Create a proof that user1 signed the transaction.
            .call_method(user1, "create_ownership_proof", args![])
            .put_last_instruction_output_on_workspace("user1_proof")
            // Withdraw the approved amount
            .call_method(owner, "withdraw_approved", args![
                Workspace("user1_proof"),
                TARI_TOKEN,
                900
            ])
            .put_last_instruction_output_on_workspace("bucket1")
            .call_method(owner, "withdraw_approved", args![
                Workspace("user1_proof"),
                TARI_TOKEN,
                100
            ])
            .put_last_instruction_output_on_workspace("bucket2")
            .drop_all_proofs_in_workspace()
            // Deposit it into user1's account
            .call_method(user1, "deposit", args![Workspace("bucket1")])
            .call_method(user1, "deposit", args![Workspace("bucket2")])
            .finish()
            .seal(&user1_secret),
        vec![user1_proof],
    );
}

#[test]
fn the_withdraw_approved_event_names_the_spending_badge() {
    let mut test = TemplateTest::new_builtin_only();

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (user1, user1_proof, user1_secret) = test.create_empty_account();

    test.execute_expect_success(
        test.transaction()
            .call_method(owner, "approve", args![user1_proof, TARI_TOKEN, 1000])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof],
    );

    let result = test.execute_expect_success(
        test.transaction()
            .call_method(user1, "create_ownership_proof", args![])
            .put_last_instruction_output_on_workspace("user1_proof")
            .call_method(owner, "withdraw_approved", args![
                Workspace("user1_proof"),
                TARI_TOKEN,
                1000
            ])
            .put_last_instruction_output_on_workspace("bucket")
            .drop_all_proofs_in_workspace()
            .call_method(user1, "deposit", args![Workspace("bucket")])
            .finish()
            .seal(&user1_secret),
        vec![user1_proof.clone()],
    );

    let event = result
        .finalize
        .events
        .iter()
        .find(|e| e.topic().ends_with("withdraw_approved"))
        .expect("no withdraw_approved event");

    // The badge is the whole non-fungible address, matching what `approve` recorded. Its resource
    // address alone does not identify which badge spent the approval.
    let payload = event.payload();
    assert_eq!(
        payload.get_as::<NonFungibleAddress>("spender_badge").unwrap(),
        Some(user1_proof)
    );
    assert_eq!(payload.get_as::<ResourceAddress>("resource").unwrap(), Some(TARI_TOKEN));
    assert_eq!(payload.get_as::<Amount>("amount").unwrap(), Some(Amount::from(1000u64)));
}

#[test]
fn it_rejects_withdrawals_greater_than_approval() {
    let mut test = TemplateTest::new_builtin_only();

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (user1, user1_proof, user1_secret) = test.create_empty_account();

    // Approve a withdrawal amount for user1
    test.execute_expect_success(
        test.transaction()
            .call_method(owner, "approve", args![user1_proof, TARI_TOKEN, 1000])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof],
    );

    // Withdraw the more than the approved amount
    let reason = test.execute_expect_failure(
        test.transaction()
            // Create a proof that user1 signed the transaction.
            .call_method(user1, "create_ownership_proof", args![])
            .put_last_instruction_output_on_workspace("user1_proof")
            // Withdraw the approved amount
            .call_method(owner, "withdraw_approved", args![
                Workspace("user1_proof"),
                TARI_TOKEN,
                1001
            ])
            .drop_all_proofs_in_workspace()
            .put_last_instruction_output_on_workspace("withdrawn_bucket")
            // Deposit it into user1's account
            .call_method(user1, "deposit", args![Workspace("withdrawn_bucket")])
            .finish()
            .seal(&user1_secret),
        vec![user1_proof],
    );

    assert_reject_reason(reason, "Amount exceeds approval");

    let (user2, user2_proof, user2_secret) = test.create_empty_account();
    // Another user tries to withdraw the approved amount
    let reason = test.execute_expect_failure(
        test.transaction()
            // Create a proof that user1 signed the transaction.
            .call_method(user2, "create_ownership_proof", args![])
            .put_last_instruction_output_on_workspace("user2_proof")
            // Withdraw the approved amount
            .call_method(owner, "withdraw_approved", args![
                Workspace("user2_proof"),
                TARI_TOKEN,
                1
            ])
            .drop_all_proofs_in_workspace()
            .put_last_instruction_output_on_workspace("withdrawn_bucket")
            // Deposit it into user1's account
            .call_method(user2, "deposit", args![Workspace("withdrawn_bucket")])
            .finish()
            .seal(&user2_secret),
        vec![user2_proof],
    );

    assert_reject_reason(reason, "No approval for badge");
}

#[test]
fn it_clears_the_approval() {
    let mut test = TemplateTest::new_builtin_only();

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (user1, user1_proof, user1_secret) = test.create_funded_account();
    // Approve a withdrawal amount for user1
    test.execute_expect_success(
        test.transaction()
            .call_method(owner, "approve", args![user1_proof, TARI_TOKEN, 1000])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof.clone()],
    );
    // Revoke all approvals
    test.execute_expect_success(
        test.transaction()
            .call_method(owner, "revoke_all_approvals", args![])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof],
    );

    // Withdraw the approved amount
    let reason = test.execute_expect_failure(
        test.transaction()
            // Create a proof that user1 signed the transaction.
            .call_method(user1, "create_ownership_proof", args![])
            .put_last_instruction_output_on_workspace("user1_proof")
            // Withdraw the approved amount
            .call_method(owner, "withdraw_approved", args![
                Workspace("user1_proof"),
                TARI_TOKEN,
                1000
            ])
            .drop_all_proofs_in_workspace()
            .put_last_instruction_output_on_workspace("withdrawn_bucket")
            // Deposit it into user1's account
            .call_method(user1, "deposit", args![Workspace("withdrawn_bucket")])
            .finish()
            .seal(&user1_secret),
        vec![user1_proof],
    );

    assert_reject_reason(reason, "No approval for badge");
}

/// Has a third party deposit `count` newly created resources into `account`, one vault each.
fn deposit_new_resources(test: &mut TemplateTest, account: ComponentAddress, count: u32) {
    let depositor = test.get_template_address("NewResourceDepositor");
    // Every deposit re-encodes the account's vault map, so batches stay small enough for the per-transaction compute
    // limit as the map grows.
    let mut remaining = count;
    while remaining > 0 {
        let batch = remaining.min(20);
        test.execute_expect_success(
            test.transaction()
                .call_function(depositor, "deposit_new_resources", args![account, batch])
                .finish()
                .seal(test.secret_key()),
            vec![],
        );
        remaining -= batch;
    }
}

fn account_vault_count(test: &mut TemplateTest, account: ComponentAddress) -> usize {
    let balances: Vec<(ResourceAddress, Amount)> = test.call_method(account, "get_balances", args![], vec![]);
    balances.len()
}

/// Returns the WASM points of a read-only balance query and of an owner withdrawal on `account`.
fn read_and_withdraw_points(
    test: &mut TemplateTest,
    account: ComponentAddress,
    proof: &NonFungibleAddress,
    secret: &RistrettoSecretKey,
    recipient: ComponentAddress,
) -> (u64, u64) {
    let _balance: Amount = test.call_method(account, "balance", args![TARI_TOKEN], vec![]);
    let read = test.last_execution_points().wasm;
    test.execute_expect_success(
        test.transaction()
            .call_method(account, "withdraw", args![TARI_TOKEN, 1])
            .put_last_instruction_output_on_workspace("bucket")
            .call_method(recipient, "deposit", args![Workspace("bucket")])
            .finish()
            .seal(secret),
        vec![proof.clone()],
    );
    (read, test.last_execution_points().wasm)
}

#[test]
fn owner_withdrawal_cost_grows_no_faster_than_a_read() {
    let mut test = TemplateTest::new(CRATE_PATH, [NEW_RESOURCE_DEPOSITOR]);

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (recipient, _, _) = test.create_empty_account();

    let (read_before, withdraw_before) =
        read_and_withdraw_points(&mut test, owner, &owner_proof, &owner_secret, recipient);
    deposit_new_resources(&mut test, owner, 200);
    let (read_after, withdraw_after) =
        read_and_withdraw_points(&mut test, owner, &owner_proof, &owner_secret, recipient);

    // Every call decodes the account's vault map, so both grow with it. A withdrawal leaves that map unchanged and
    // must not also pay to re-encode it.
    let read_growth = read_after - read_before;
    let withdraw_growth = withdraw_after - withdraw_before;
    assert!(
        withdraw_growth <= read_growth,
        "withdraw grew by {withdraw_growth} points, a read by {read_growth}"
    );
}

#[test]
fn third_party_deposits_cannot_grow_an_account_past_the_vault_limit() {
    let mut test = TemplateTest::new(CRATE_PATH, [NEW_RESOURCE_DEPOSITOR]);
    let depositor = test.get_template_address("NewResourceDepositor");

    let (owner, owner_proof, owner_secret) = test.create_funded_account();
    let (recipient, _, _) = test.create_empty_account();
    let initial_vaults = u32::try_from(account_vault_count(&mut test, owner)).unwrap();
    deposit_new_resources(&mut test, owner, MAX_ACCOUNT_VAULTS - initial_vaults);
    assert_eq!(account_vault_count(&mut test, owner), MAX_ACCOUNT_VAULTS as usize);

    let reason = test.execute_expect_failure(
        test.transaction()
            .call_function(depositor, "deposit_new_resources", args![owner, 1])
            .finish()
            .seal(test.secret_key()),
        vec![],
    );
    assert_reject_reason(reason, "Account holds the maximum of 1024 vaults");
    assert_eq!(account_vault_count(&mut test, owner), MAX_ACCOUNT_VAULTS as usize);

    // A full account still pays fees, sends, and receives resources it already holds.
    test.enable_fees();
    test.execute_expect_success(
        test.transaction()
            .pay_fee_from_component(owner, 1_000_000u64)
            .call_method(owner, "withdraw", args![TARI_TOKEN, 1000])
            .put_last_instruction_output_on_workspace("sent")
            .call_method(recipient, "deposit", args![Workspace("sent")])
            .call_method(owner, "withdraw", args![TARI_TOKEN, 10])
            .put_last_instruction_output_on_workspace("kept")
            .call_method(owner, "deposit", args![Workspace("kept")])
            .finish()
            .seal(&owner_secret),
        vec![owner_proof],
    );
}
