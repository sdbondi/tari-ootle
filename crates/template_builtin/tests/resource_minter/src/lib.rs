//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Mints and burns arbitrary resources from outside any component, standing in for an unrelated third-party
/// template.
#[template]
mod resource_minter_template {
    use super::*;

    pub struct ResourceMinter;

    impl ResourceMinter {
        pub fn mint(resource: ResourceAddress, amount: Amount) -> Bucket {
            ResourceManager::get(resource).mint_fungible(amount)
        }

        pub fn burn(bucket: Bucket) {
            bucket.burn();
        }
    }
}
