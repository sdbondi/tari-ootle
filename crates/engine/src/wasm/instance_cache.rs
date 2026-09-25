//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! The WASM instances one transaction has created, kept so a template's later calls in the
//! transaction run on the instance its first call created.
//!
//! The cache is scoped to one transaction and dropped with it. Guest statics and heap therefore
//! persist across a template's calls within a transaction and never across transactions. Reusing an
//! instance is safe only because any failed call rejects the whole transaction; an instance whose
//! call failed is still discarded rather than returned, so no later call can run on an instance a
//! trap left mid-way.

use std::collections::HashMap;

use tari_template_lib::types::TemplateAddress;
use wasmer::Store;

use crate::wasm::WasmProcess;

/// A template instance together with the store that owns it.
pub struct WasmInstance {
    pub store: Store,
    pub process: WasmProcess,
}

/// What [`WasmInstanceCache::checkout`] found for a template.
#[allow(clippy::large_enum_variant)]
pub enum Checkout {
    /// The template's instance, idle since its previous call. Return it with
    /// [`WasmInstanceCache::checkin`].
    Reuse(WasmInstance),
    /// No instance yet. The call creates one and returns it with [`WasmInstanceCache::checkin`].
    Vacant,
    /// The template's instance is running a call further up the stack. The call creates an
    /// instance of its own and discards it afterwards, leaving the running instance as the
    /// template's cached instance.
    Reentrant,
}

#[derive(Default)]
pub struct WasmInstanceCache {
    slots: HashMap<TemplateAddress, Slot>,
}

#[allow(clippy::large_enum_variant)]
enum Slot {
    Idle(WasmInstance),
    InUse,
}

impl WasmInstanceCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the template's instance for a call. Unless the result is [`Checkout::Reentrant`], the
    /// slot stays in use until the call ends with [`Self::checkin`] or [`Self::discard`].
    pub fn checkout(&mut self, template_address: &TemplateAddress) -> Checkout {
        match self.slots.insert(*template_address, Slot::InUse) {
            Some(Slot::Idle(instance)) => Checkout::Reuse(instance),
            None => Checkout::Vacant,
            Some(Slot::InUse) => Checkout::Reentrant,
        }
    }

    /// Returns an instance whose call succeeded, for the template's next call.
    pub fn checkin(&mut self, template_address: &TemplateAddress, instance: WasmInstance) {
        self.slots.insert(*template_address, Slot::Idle(instance));
    }

    /// Ends a call that failed. Its instance is not returned, so the template's next call in the
    /// transaction (if any) instantiates afresh.
    pub fn discard(&mut self, template_address: &TemplateAddress) {
        self.slots.remove(template_address);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: TemplateAddress = TemplateAddress::from_array([1; 32]);

    #[test]
    fn a_call_inside_a_running_call_is_reentrant_until_the_outer_call_ends() {
        let mut cache = WasmInstanceCache::new();
        assert!(matches!(cache.checkout(&TEMPLATE), Checkout::Vacant));
        assert!(matches!(cache.checkout(&TEMPLATE), Checkout::Reentrant));
        // The inner call ending leaves the outer call's slot in use.
        assert!(matches!(cache.checkout(&TEMPLATE), Checkout::Reentrant));
    }

    #[test]
    fn a_discarded_call_leaves_the_template_vacant() {
        let mut cache = WasmInstanceCache::new();
        assert!(matches!(cache.checkout(&TEMPLATE), Checkout::Vacant));
        cache.discard(&TEMPLATE);
        assert!(matches!(cache.checkout(&TEMPLATE), Checkout::Vacant));
    }
}
