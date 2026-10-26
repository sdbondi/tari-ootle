//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Transaction signatures.
//!
//! Transaction authorization and seal signatures are standard sr25519 signatures
//! ([`Sr25519SignatureBytes`]) under [`TRANSACTION_SIGNING_CONTEXT`]. sr25519 keys are Ristretto255
//! keys, so the keys tari_crypto manages sign directly, and off-the-shelf sr25519 signers (HSMs, MPC
//! vaults, Polkadot wallets) produce signatures that verify here unchanged.

use tari_crypto::ristretto::{RistrettoPublicKey, RistrettoSecretKey};
pub use tari_ootle_sr25519::{SigningContext, Sr25519SignatureBytes, components, from_components};
use tari_template_lib_types::crypto::RistrettoPublicKeyBytes;

/// The sr25519 signing context of every transaction authorization and seal signature.
pub const TRANSACTION_SIGNING_CONTEXT: TransactionSigningContext =
    TransactionSigningContext(SigningContext::new(b"substrate"));

/// A transaction signing message: the 64-byte domain-separated digest of a transaction.
pub type TransactionMessage = [u8; 64];

/// sr25519 under schnorrkel's default context, the only one many stock sr25519 signers support,
/// restricted to 64-byte messages.
///
/// Separation from Substrate chains comes from the message length. A standard Substrate signing payload is
/// either longer than 64 bytes, since it ends with the 32-byte genesis and block hashes after the call,
/// or hashed to 32 bytes when longer than 256. Every signing call here takes exactly 64 bytes, so a key
/// signing through it can never be made to sign a Substrate extrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionSigningContext(SigningContext<'static>);

impl TransactionSigningContext {
    pub const fn as_bytes(&self) -> &'static [u8] {
        self.0.as_bytes()
    }

    /// Signs a transaction `message` with `secret_key`.
    pub fn sign(&self, secret_key: &RistrettoSecretKey, message: &TransactionMessage) -> Sr25519SignatureBytes {
        self.0.sign(secret_key, message)
    }

    /// True when `signature` is `public_key`'s signature over `message`. See [`SigningContext::verify`].
    pub fn verify(
        &self,
        signature: &Sr25519SignatureBytes,
        public_key: &RistrettoPublicKeyBytes,
        message: &TransactionMessage,
    ) -> bool {
        self.0.verify(signature, public_key, message)
    }

    /// True when every item verifies. See [`SigningContext::verify_batch`].
    pub fn verify_batch(
        &self,
        items: &[(&Sr25519SignatureBytes, &RistrettoPublicKeyBytes, &TransactionMessage)],
    ) -> bool {
        self.0.verify_batch(items)
    }

    /// The challenge scalar of a signature over `message`. See [`SigningContext::challenge`].
    pub fn challenge(
        &self,
        public_nonce: &RistrettoPublicKey,
        public_key: &RistrettoPublicKey,
        message: &TransactionMessage,
    ) -> RistrettoSecretKey {
        self.0.challenge(public_nonce, public_key, message)
    }
}

#[derive(Debug, Clone)]
pub struct SignatureOutput {
    pub signature: Sr25519SignatureBytes,
    pub public_key: RistrettoPublicKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stock sr25519 signer's signature verifies as a transaction signature: this one is from
    /// `@scure/sr25519` 2.4.0 under its default context, over the 64-byte message `0x00..0x3f`.
    #[test]
    fn a_stock_sr25519_signature_verifies_under_the_transaction_context() {
        let message = std::array::from_fn::<u8, 64, _>(|i| i as u8);
        let public_key =
            RistrettoPublicKeyBytes::from_hex("ccddafdb8188e3edb0984780053ac7f357f1741c59292884d9fe3e5832d2f90f")
                .unwrap();
        let signature: Sr25519SignatureBytes =
            "d8280e79fe66ee85d9f20cdbfccff261bbb7401140d098a54ec1b046bdfd670a50fa8e16adbe36b86fc1ac6e4c86921eae2817b5f86fdec12424d482077abc84"
                .parse()
                .unwrap();

        assert!(TRANSACTION_SIGNING_CONTEXT.verify(&signature, &public_key, &message));
    }
}
