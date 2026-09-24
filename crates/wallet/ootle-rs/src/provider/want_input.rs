//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_common_types::engine_types::substate::SubstateId;
use tari_template_lib_types::{ComponentAddress, ResourceAddress};

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum WantInput {
    /// A specific vault for a given resource from a component
    /// The resolver will fetch the component, inspect it's state and extract the vaults, then attempt to find the
    /// vault for the given resource address.
    ///
    /// The vault is declared a write and its resource a read, which covers deposits and withdrawals. A
    /// transaction that alters the resource itself (a mint or burn on a resource that tracks supply, or a
    /// change to its rules or metadata) must also want it as a [`WantInput::SpecificSubstate`].
    VaultForResource {
        component_address: ComponentAddress,
        resource_address: ResourceAddress,
        required: bool,
    },
    /// Adds a substate as an input. If required is false, the resolver only add the input if it was found.
    SpecificSubstate { substate_id: SubstateId, required: bool },
    /// Fetches the component state and adds ALL vaults found in it as inputs.
    /// Used by the generic component builder when the specific vault access pattern is unknown. The
    /// called method may alter the vaults' resources, so they are declared as writes.
    AllComponentVaults { component_address: ComponentAddress },
}
