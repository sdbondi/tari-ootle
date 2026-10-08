//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Deposits freshly created resources into an account it does not own, standing in for an unrelated third party.
#[template]
mod new_resource_depositor_template {
    use super::*;

    pub struct NewResourceDepositor;

    impl NewResourceDepositor {
        /// Creates `count` new single-unit resources and deposits each into `account`, so that every deposit adds a
        /// vault to it.
        pub fn deposit_new_resources(account: ComponentAddress, count: u32) {
            let account = ComponentManager::get(account);
            for _ in 0..count {
                let bucket = ResourceBuilder::public_fungible().initial_supply(1u32);
                account.invoke("deposit", args![bucket]);
            }
        }
    }
}
