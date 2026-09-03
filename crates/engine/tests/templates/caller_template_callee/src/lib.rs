//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Callee template: `bar` is restricted to callers from the template whose address is passed as `gate`.
#[template]
mod callee {
    use super::*;

    pub struct TemplateCallee;

    impl TemplateCallee {
        pub fn new(gate: TemplateAddress) -> Component<Self> {
            let access_rules = ComponentAccessRules::new()
                .method("bar", rule!(direct_caller_template(gate)))
                .method("ping", rule!(allow_all))
                .default(rule!(deny_all));

            Component::new(TemplateCallee)
                .with_access_rules(access_rules)
                .with_owner_rule(OwnerRule::None)
                .create()
        }

        pub fn bar(&self) {}

        pub fn ping(&self) -> u64 {
            42
        }
    }
}
