//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_common_types::Epoch;
use tari_template_lib_types::TemplateAddress;

use crate::{
    AllocatableAddressType,
    ComponentReference,
    Instruction,
    Transaction,
    args,
    args::{InstructionArg, WorkspaceOffsetId},
    builder::named_component_call::CallFromWorkspace,
};

#[test]
fn it_converts_workspace_names_to_ids() {
    let transaction = Transaction::builder_localnet(Epoch(1))
        .put_last_instruction_output_on_workspace("thing1")
        .allocate_resource_address("thing2")
        .allocate_component_address("thing3")
        .call_function(TemplateAddress::default(), "do_something", args![
            Workspace("thing1"),
            "thing2",
            Workspace("thing1.0"),
            Workspace("thing2.2")
        ])
        .call_method(CallFromWorkspace::new("thing3"), "do_something_else", args![Workspace(
            "thing1"
        )])
        .build_unsigned();

    assert_eq!(
        transaction.instructions()[0],
        Instruction::PutLastInstructionOutputOnWorkspace { key: 0 }
    );
    assert_eq!(transaction.instructions()[1], Instruction::AllocateAddress {
        allocatable_type: AllocatableAddressType::Resource,
        workspace_id: 1,
    });
    assert_eq!(transaction.instructions()[2], Instruction::AllocateAddress {
        allocatable_type: AllocatableAddressType::Component,
        workspace_id: 2,
    });
    assert_eq!(transaction.instructions()[3], Instruction::CallFunction {
        address: TemplateAddress::default(),
        function: "do_something".try_into().unwrap(),
        args: vec![
            InstructionArg::Workspace(WorkspaceOffsetId::new(0)),
            InstructionArg::from_type(&"thing2").unwrap(),
            InstructionArg::Workspace(WorkspaceOffsetId::new(0).with_offset(0)),
            InstructionArg::Workspace(WorkspaceOffsetId::new(1).with_offset(2))
        ]
    });
    assert_eq!(transaction.instructions()[4], Instruction::CallMethod {
        call: ComponentReference::Workspace(2),
        method: "do_something_else".try_into().unwrap(),
        args: vec![InstructionArg::Workspace(WorkspaceOffsetId::new(0))]
    });
}

/// Merge must remap blob indices and append blobs from `other` so references stay valid.
#[test]
fn merge_remaps_blob_ids_and_appends_blobs() {
    let address = TemplateAddress::from_array([7; 32]);

    // First builder owns one blob `a` referenced by an arg.
    let a = Transaction::builder_localnet(Epoch(1))
        .add_blob("a", vec![1u8, 2, 3])
        .call_function(address, "f", args![Blob("a")]);

    // Second builder owns its own blob `b`.
    let b = Transaction::builder_localnet(Epoch(1))
        .add_blob("b", vec![4u8, 5])
        .call_function(address, "g", args![Blob("b")]);

    let merged = a.merge(b).build_unsigned();

    // Both blobs are present, in order.
    let blobs = merged.blobs();
    assert_eq!(blobs.len(), 2);
    assert_eq!(blobs.get(0).unwrap().as_bytes(), &[1u8, 2, 3]);
    assert_eq!(blobs.get(1).unwrap().as_bytes(), &[4u8, 5]);

    // The first instruction's Blob arg still references index 0 (no shift, it was already
    // on `self`); the second's Blob arg has been shifted from 0 → 1 during merge.
    assert_eq!(merged.instructions()[0], Instruction::CallFunction {
        address,
        function: "f".try_into().unwrap(),
        args: vec![InstructionArg::Blob(0)],
    });
    assert_eq!(merged.instructions()[1], Instruction::CallFunction {
        address,
        function: "g".try_into().unwrap(),
        args: vec![InstructionArg::Blob(1)],
    });
}

#[test]
#[should_panic(expected = "blob name 'a' collides during merge")]
fn merge_rejects_colliding_blob_names() {
    let a = Transaction::builder_localnet(Epoch(1)).add_blob("a", vec![1u8]);
    let b = Transaction::builder_localnet(Epoch(1)).add_blob("a", vec![2u8]);
    let _unused = a.merge(b);
}

#[test]
fn merge_remaps_publish_template_blob_index() {
    // `self` already has a blob, so the merged builder's auto-added template blob index 0
    // becomes index 1 after merge.
    let a = Transaction::builder_localnet(Epoch(1)).add_blob("filler", vec![0u8; 4]);
    let b = Transaction::builder_localnet(Epoch(1)).publish_template(vec![9u8, 9, 9]);

    let merged = a.merge(b).build_unsigned();

    let blobs = merged.blobs();
    assert_eq!(blobs.len(), 2);
    assert_eq!(blobs.get(0).unwrap().as_bytes(), &[0u8; 4][..]);
    assert_eq!(blobs.get(1).unwrap().as_bytes(), &[9u8, 9, 9]);

    assert_eq!(merged.instructions()[0], Instruction::PublishTemplate {
        binary: 1,
        metadata_hash: None,
    });
}

/// The fee builder indexes its blobs from zero, independently of the main builder's. Carrying its
/// instructions across without its blobs leaves them pointing at whatever happens to sit at that
/// index in the main list, or at nothing at all.
#[test]
fn fee_instruction_blobs_are_carried_over_and_remapped() {
    let tx = Transaction::builder_localnet(Epoch(1))
        .add_blob("main", vec![0u8; 4])
        .with_fee_instructions_builder(|builder| builder.publish_template(vec![9u8, 9, 9]))
        .build_unsigned();

    let blobs = tx.blobs();
    assert_eq!(blobs.len(), 2, "the fee builder's blob was dropped");
    assert_eq!(blobs.get(0).unwrap().as_bytes(), &[0u8; 4][..]);
    assert_eq!(blobs.get(1).unwrap().as_bytes(), &[9u8, 9, 9]);

    assert_eq!(tx.fee_instructions()[0], Instruction::PublishTemplate {
        binary: 1,
        metadata_hash: None,
    });
}

/// A blob referenced only from a fee instruction still has to resolve.
#[test]
fn a_fee_instruction_blob_resolves_when_nothing_else_carries_one() {
    let tx = Transaction::builder_localnet(Epoch(1))
        .with_fee_instructions_builder(|builder| builder.publish_template(vec![4u8, 5, 6]))
        .build_unsigned();

    assert_eq!(tx.blobs().len(), 1);
    assert_eq!(tx.fee_instructions()[0], Instruction::PublishTemplate {
        binary: 0,
        metadata_hash: None,
    });
}

/// A transaction may carry `BlobIndex::MAX + 1` blobs, so the count itself does not fit a
/// `BlobIndex`. Building one with a full blob list must still work when the fee builder adds none.
#[test]
fn a_full_blob_list_builds_when_the_fee_builder_carries_none() {
    let mut builder = Transaction::builder_localnet(Epoch(1));
    for i in 0..=u8::MAX {
        builder = builder.add_blob(format!("b{i}"), vec![i]);
    }

    let tx = builder
        .with_fee_instructions_builder(|b| b.add_instruction(Instruction::DropAllProofsInWorkspace))
        .build_unsigned();

    assert_eq!(tx.blobs().len(), u8::MAX as usize + 1);
}

/// Fee and main blobs share one `BlobIndex` range, so the sum is what a new blob has to fit. A
/// caller filling both halves through the fallible API must be told which addition does not fit,
/// whichever half it is on and whichever order the halves are built in — `finish` is infallible and
/// has nowhere to report it.
#[test]
fn the_fallible_api_refuses_the_blob_that_does_not_fit() {
    // Main filled first: the fee builder's addition is the one refused.
    let mut builder = Transaction::builder_localnet(Epoch(1));
    for i in 0..=u8::MAX {
        builder = builder.add_blob(format!("b{i}"), vec![i]);
    }
    let mut refused_on_fee = false;
    let builder = builder.with_fee_instructions_builder(|b| {
        // `add_blob_checked` consumes the builder, so probe with a clone to keep it on refusal.
        match b.clone().add_blob_checked("fee", vec![1]) {
            Ok(b) => b,
            Err(_) => {
                refused_on_fee = true;
                b
            },
        }
    });
    assert!(refused_on_fee, "a 257th blob on the fee builder must be refused");
    assert_eq!(builder.build_unsigned().blobs().len(), u8::MAX as usize + 1);

    // Fee builder filled first: the main builder's addition is the one refused.
    let builder = Transaction::builder_localnet(Epoch(1)).with_fee_instructions_builder(|mut b| {
        for i in 0..=u8::MAX {
            b = b.add_blob(format!("f{i}"), vec![i]);
        }
        b
    });
    assert!(builder.add_blob_checked("main", vec![0]).is_err());
}
