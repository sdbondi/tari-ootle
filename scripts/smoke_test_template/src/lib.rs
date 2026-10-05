//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Template published by `scripts/smoke_test.py`. `SMOKE_TEST_NONCE` is baked in at build time so
//! every run publishes a distinct binary, and therefore a distinct template address.

use tari_template_lib::prelude::*;

const NONCE: &str = match option_env!("SMOKE_TEST_NONCE") {
    Some(nonce) => nonce,
    None => "dev",
};

#[template]
mod smoke_token {
    use super::*;

    pub struct SmokeToken {
        count: u64,
    }

    impl SmokeToken {
        /// Creates a new stealth resource and returns its whole initial supply as revealed funds.
        pub fn mint(symbol: String, initial_supply: Amount) -> Bucket {
            ResourceBuilder::stealth()
                .with_token_symbol(symbol)
                .initial_supply(initial_supply)
        }

        /// Creates a resource with no supply and drops its address. The engine must reject this.
        pub fn create_orphan(symbol: String) {
            let _address = ResourceBuilder::stealth().with_token_symbol(symbol).build();
        }

        pub fn new() -> Component<Self> {
            Component::new(Self { count: 0 })
                .with_access_rules(ComponentAccessRules::allow_all())
                .create()
        }

        pub fn increment(&mut self) -> u64 {
            self.count += 1;
            self.count
        }

        /// Panics unless the counter equals `expected`, so a committed call proves the state that
        /// earlier transactions wrote.
        pub fn assert_count(&self, expected: u64) {
            assert_eq!(self.count, expected, "counter mismatch");
        }

        pub fn nonce() -> String {
            NONCE.to_string()
        }
    }
}
