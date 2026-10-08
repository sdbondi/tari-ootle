//  Copyright 2022 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

use std::{ops::ControlFlow, time::Duration};

use cucumber::{gherkin::Step, given, then};
use integration_tests::{base_node::spawn_base_node, wait::wait_until};

use crate::TariWorld;

#[given(expr = "a base node {word}")]
async fn start_base_node(world: &mut TariWorld, step: &Step, bn_name: String) {
    integration_tests::cucumber_log!("==== Step: {}", step.value);
    spawn_base_node(world, bn_name).await;
}

#[then(expr = "there is {int} transaction in the mempool of {word} within {int} seconds")]
async fn then_there_is_transaction_in_the_mempool_of(
    world: &mut TariWorld,
    step: &Step,
    num_tx: usize,
    node_name: String,
    seconds: u64,
) {
    integration_tests::cucumber_log!("==== Step: {}", step.value);
    let node = world.get_base_node(&node_name);
    let mut client = node.create_client();
    wait_until(Duration::from_secs(seconds), async || {
        let mempool_count = client
            .get_mempool_transaction_count()
            .await
            .expect("failed to get mempool count");
        if mempool_count == num_tx {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(mempool_count)
        }
    })
    .await
    .unwrap_or_else(|err| panic!("Base node {node_name} does not have {num_tx} transaction(s) in its mempool: {err}"));
}
