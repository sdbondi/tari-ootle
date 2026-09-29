//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_transaction::Network;
use tari_template_lib::types::Amount;

/// The tTARI held by the testnet faucet at genesis.
///
/// Public testnets start with an empty faucet: tTARI enters circulation through burn claims, and anyone may top the
/// faucet up by calling its `deposit` method. LocalNet mints just under 18.5 trillion so that local swarms and
/// integration tests can fund accounts without an L1 burn.
pub const fn txtr_faucet_initial_supply(network: Network) -> Amount {
    match network {
        Network::LocalNet => Amount::from_u64(u64::MAX),
        _ => Amount::zero(),
    }
}
