//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Observes what a template call can see of the calls before it on the same instance: a guest
//! static counting calls, the size of linear memory, and bytes an earlier call wrote to memory.

use core::sync::atomic::{AtomicU32, Ordering};

use tari_template_lib::prelude::*;

static CALLS: AtomicU32 = AtomicU32::new(0);

/// The last byte of linear memory, which a freshly instantiated template holds as zero.
fn last_byte() -> *mut u8 {
    (core::arch::wasm32::memory_size(0) * 64 * 1024 - 1) as *mut u8
}

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

        /// Grows linear memory by one page and returns its size, in pages, before the growth.
        pub fn grow_memory() -> u32 {
            let before = core::arch::wasm32::memory_size(0);
            core::arch::wasm32::memory_grow(0, 1);
            before as u32
        }

        /// Writes a non-zero byte to the last byte of linear memory.
        pub fn scribble_last_byte() {
            unsafe { last_byte().write_volatile(0xA5) }
        }

        /// Reads the last byte of linear memory.
        pub fn read_last_byte() -> u8 {
            unsafe { last_byte().read_volatile() }
        }
    }
}
