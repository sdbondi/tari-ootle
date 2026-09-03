//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::prelude::*;

/// Caller template: performs cross-component calls into a callee's methods.
#[template]
mod caller {
    use super::*;

    pub struct TemplateCaller;

    impl TemplateCaller {
        pub fn new() -> Component<Self> {
            let access_rules = ComponentAccessRules::new()
                .method("call_bar", rule!(allow_all))
                .method("call_ping", rule!(allow_all))
                .default(rule!(deny_all));

            Component::new(TemplateCaller)
                .with_access_rules(access_rules)
                .with_owner_rule(OwnerRule::None)
                .create()
        }

        pub fn call_bar(&self, callee: ComponentAddress) {
            ComponentManager::get(callee).invoke("bar", args![]);
        }

        pub fn call_ping(&self, callee: ComponentAddress) -> u64 {
            ComponentManager::get(callee).call("ping", args![])
        }

        // Static (template-function) callers have no component instance, only a template identity.
        pub fn call_bar_static(callee: ComponentAddress) {
            ComponentManager::get(callee).invoke("bar", args![]);
        }

        pub fn call_ping_static(callee: ComponentAddress) -> u64 {
            ComponentManager::get(callee).call("ping", args![])
        }
    }
}
