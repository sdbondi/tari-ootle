//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Observes the lifetime of the WASM instance serving a template's calls: a guest static counts the
//! calls one instance has served, so its value tells the caller which instance ran.

use core::sync::atomic::{AtomicU32, Ordering};

use tari_template_lib::prelude::*;

static CALLS: AtomicU32 = AtomicU32::new(0);

#[template]
mod instance_reuse {
    use super::*;

    pub struct InstanceReuse {}

    impl InstanceReuse {
        /// Returns how many calls, this one included, the serving instance has handled.
        pub fn count() -> u32 {
            CALLS.fetch_add(1, Ordering::Relaxed) + 1
        }

        /// Counts this call, then calls `count` on this same template from inside it. Returns the outer and
        /// inner counts.
        pub fn count_reentrant(this_template: TemplateAddress) -> (u32, u32) {
            let outer = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
            let inner: u32 = TemplateManager::get(this_template).call("count", args![]);
            (outer, inner)
        }

        /// Reports a panic message through `on_panic` and returns normally.
        pub fn plant_panic_message() {
            let msg = b"planted by an earlier call";
            unsafe { tari_template_abi::on_panic(msg.as_ptr(), msg.len() as u32, 0, 0) }
        }

        /// Counts this call, then traps without reporting a panic message.
        pub fn count_then_trap() {
            CALLS.fetch_add(1, Ordering::Relaxed);
            core::arch::wasm32::unreachable()
        }
    }
}
