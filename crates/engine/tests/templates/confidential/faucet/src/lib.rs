//   Copyright 2022. The Tari Project
//
//   Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//   following conditions are met:
//
//   1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//   disclaimer.
//
//   2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//   following disclaimer in the documentation and/or other materials provided with the distribution.
//
//   3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//   products derived from this software without specific prior written permission.
//
//   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//   INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//   DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//   SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//   WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//   USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use tari_template_abi::rust::collections::BTreeMap;
use tari_template_lib::prelude::*;

#[template]
mod faucet_template {
    use super::*;

    pub struct ConfidentialFaucet {
        vault: Vault,
        hook_calls: u64,
    }

    impl ConfidentialFaucet {
        pub fn mint(
            confidential_proof: ConfidentialOutputStatement,
            value_proofs: BTreeMap<PedersenCommitmentBytes, crypto::CommitmentValueProof>,
        ) -> Component<Self> {
            let coins = ResourceBuilder::confidential()
                .mintable(rule!(allow_all), OWNER)
                .burnable(rule!(allow_all), OWNER)
                .initial_supply_with_value_proofs(confidential_proof, value_proofs);

            Component::new(Self {
                vault: Vault::from_bucket(coins),
                hook_calls: 0,
            })
            .with_access_rules(AccessRules::allow_all())
            .create()
        }

        pub fn mint_with_view_key(
            confidential_proof: ConfidentialOutputStatement,
            view_key: RistrettoPublicKeyBytes,
            value_proofs: BTreeMap<PedersenCommitmentBytes, crypto::CommitmentValueProof>,
        ) -> Component<Self> {
            let coins = ResourceBuilder::confidential()
                .mintable(rule!(allow_all), OWNER)
                .burnable(rule!(allow_all), OWNER)
                .with_view_key(view_key)
                .initial_supply_with_value_proofs(confidential_proof, value_proofs);

            Component::new(Self {
                vault: Vault::from_bucket(coins),
                hook_calls: 0,
            })
            .with_access_rules(AccessRules::allow_all())
            .create()
        }

        /// Like [`mint`], but the resource binds [`count_auth_hook`] on this component as its authorization hook.
        pub fn mint_with_auth_hook(
            confidential_proof: ConfidentialOutputStatement,
            value_proofs: BTreeMap<PedersenCommitmentBytes, crypto::CommitmentValueProof>,
        ) -> Component<Self> {
            let alloc = CallerContext::allocate_component_address(None);
            let coins = ResourceBuilder::confidential()
                .mintable(rule!(allow_all), OWNER)
                .freezable(rule!(allow_all), OWNER)
                .with_authorization_hook(alloc.get_address(), "count_auth_hook")
                .initial_supply_with_value_proofs(confidential_proof, value_proofs);

            Component::new(Self {
                vault: Vault::from_bucket(coins),
                hook_calls: 0,
            })
            .with_address_allocation(alloc)
            .with_access_rules(AccessRules::allow_all())
            .create()
        }

        /// Records each invocation. The engine skips the hook when this component itself acts on the resource, so
        /// [`freeze_confidential_outputs_of`] is what exercises it.
        pub fn count_auth_hook(&mut self, _action: ResourceAuthAction, caller: AuthHookCaller) {
            assert_eq!(
                *caller.resource(),
                self.vault.resource_address(),
                "hook invoked for a foreign resource"
            );
            self.hook_calls += 1;
        }

        pub fn hook_calls(&self) -> u64 {
            self.hook_calls
        }

        pub fn freeze_confidential_outputs_of(resource: ResourceAddress, commitments: Vec<PedersenCommitmentBytes>) {
            ResourceManager::get(resource).freeze_confidential_outputs(commitments);
        }

        pub fn mint_revealed(&mut self, amount: Amount) {
            let proof = ConfidentialOutputStatement::mint_revealed(amount);
            let bucket = ResourceManager::get(self.vault.resource_address()).mint_confidential(proof, BTreeMap::new());
            self.vault.deposit(bucket);
        }

        pub fn mint_revealed_with_bad_range_proof(&mut self, amount: Amount) {
            let mut proof = ConfidentialOutputStatement::mint_revealed(amount);
            proof.range_proof = crypto::RangeProofBytes::try_from(vec![1, 2, 3]).unwrap();
            let bucket = ResourceManager::get(self.vault.resource_address()).mint_confidential(proof, BTreeMap::new());
            self.vault.deposit(bucket);
        }

        pub fn mint_more(
            &mut self,
            proof: ConfidentialOutputStatement,
            value_proofs: BTreeMap<PedersenCommitmentBytes, crypto::CommitmentValueProof>,
        ) {
            let bucket = ResourceManager::get(self.vault.resource_address()).mint_confidential(proof, value_proofs);
            self.vault.deposit(bucket);
        }

        pub fn take_free_coins(&mut self, proof: ConfidentialWithdrawProof) -> Bucket {
            debug!(
                "Withdrawing {} revealed coins from faucet and {} commitments",
                proof.revealed_input_amount(),
                proof.inputs.len()
            );
            self.vault.withdraw_confidential(proof)
        }

        pub fn freeze_confidential_outputs(&self, commitments: Vec<PedersenCommitmentBytes>) {
            ResourceManager::get(self.vault.resource_address()).freeze_confidential_outputs(commitments);
        }

        pub fn unfreeze_confidential_outputs(&self, commitments: Vec<PedersenCommitmentBytes>) {
            ResourceManager::get(self.vault.resource_address()).unfreeze_confidential_outputs(commitments);
        }

        pub fn total_supply(&self) -> Option<Amount> {
            ResourceManager::get(self.vault.resource_address()).total_supply_opt()
        }

        /// Utility function for tests
        pub fn split_coins(mut bucket: Bucket, proof: ConfidentialWithdrawProof) -> (Bucket, Bucket) {
            let new_bucket = bucket.take_confidential(proof);
            (new_bucket, bucket)
        }

        pub fn vault_balance(&self) -> Amount {
            self.vault.balance()
        }
    }
}
