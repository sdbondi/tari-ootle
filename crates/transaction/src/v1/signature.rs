//    Copyright 2024 The Tari Project
//    SPDX-License-Identifier: BSD-3-Clause

use blake2::Blake2b;
use curve25519_dalek::{
    RistrettoPoint,
    Scalar,
    constants::RISTRETTO_BASEPOINT_POINT,
    traits::{Identity, VartimeMultiscalarMul},
};
use digest::consts::U64;
use indexmap::IndexSet;
use ootle_byte_type::{ConvertFromByteType, FromByteType, ToByteType};
use tari_crypto::{
    keys::{PublicKey as PublicKeyT, SecretKey},
    ristretto::{RistrettoPublicKey, RistrettoSchnorr, RistrettoSecretKey},
    tari_utilities,
    tari_utilities::ByteArray,
};
use tari_ootle_common_types::{Epoch, SubstateRequirement, signature::SignatureOutput};
use tari_template_lib_types::crypto::{RistrettoPublicKeyBytes, SchnorrSignatureBytes};

use crate::{
    BlobHashes,
    Instruction,
    UnsealedTransactionV1,
    UnsignedTransaction,
    UnsignedTransactionV1,
    hashing::transaction_hasher_v1,
    unsealed::UnsealedTransaction,
    v1::pruned::{PrunedUnsealedTransactionV1, PrunedUnsignedTransactionV1},
};

/// Identifies a field of the canonical signing preimage, in chain order.
///
/// The discriminants are the on-the-wire field tags used to stream a transaction to a hardware
/// signer (e.g. the Ootle Ledger app). The signer reconstructs the exact byte sequence chained
/// into the message digest by concatenating each segment's bytes in ascending field order
/// (`SealSigner` only present for an authorization signature; `Signatures` only for a seal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PreimageField {
    SchemaVersion = 0,
    SealSigner = 1,
    Network = 2,
    FeeInstructions = 3,
    Instructions = 4,
    Inputs = 5,
    MinEpoch = 6,
    MaxEpoch = 7,
    IsSealSignerAuthorized = 8,
    DryRun = 9,
    Nonce = 10,
    BlobHashes = 11,
    Signatures = 12,
}

/// One ordered segment of the canonical signing preimage: the exact borsh bytes chained into the
/// message digest for `field`. Concatenating every segment's `bytes` in order yields the precise
/// byte sequence that `create_message_v1` feeds into the domain-separated hasher (after the
/// domain-separation preamble, which the signer prepends itself).
#[derive(Debug, Clone)]
pub struct PreimageSegment {
    pub field: PreimageField,
    pub bytes: Vec<u8>,
}

fn preimage_segment<T: borsh::BorshSerialize + ?Sized>(field: PreimageField, value: &T) -> PreimageSegment {
    PreimageSegment {
        field,
        // BorshSerialize is infallible for these types; the hasher relies on the same guarantee.
        bytes: borsh::to_vec(value).expect("BorshSerialize is infallible for transaction fields"),
    }
}

#[derive(Debug, Clone, Eq, PartialEq, borsh::BorshSerialize, minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TransactionSealSignature {
    #[n(0)]
    public_key: RistrettoPublicKeyBytes,
    #[n(1)]
    signature: SchnorrSignatureBytes,
}

impl TransactionSealSignature {
    pub fn new(public_key: RistrettoPublicKeyBytes, signature: SchnorrSignatureBytes) -> Self {
        Self { public_key, signature }
    }

    pub fn sign_v1(secret_key: &RistrettoSecretKey, transaction: &UnsealedTransactionV1) -> Self {
        let public_key = RistrettoPublicKey::from_secret_key(secret_key);

        let message = Self::create_message_v1(transaction);
        Self {
            signature: RistrettoSchnorr::sign(secret_key, message, &mut rand::rng())
                .expect("sign is infallible with Ristretto keys")
                .to_byte_type(),
            public_key: public_key.to_byte_type(),
        }
    }

    pub fn verify(&self, transaction: &UnsealedTransaction) -> bool {
        match transaction {
            UnsealedTransaction::V1(t) => self.verify_v1(t),
        }
    }

    pub fn verify_v1(&self, transaction: &UnsealedTransactionV1) -> bool {
        self.verify_message(Self::create_message_v1(transaction))
    }

    /// Verifies the seal against a transaction whose blob commitments have already been derived.
    ///
    /// Deriving them hashes every blob payload, so a caller that also verifies the authorization
    /// signatures should derive [`BlobHashes`] once and use this for both.
    pub fn verify_v1_with_blob_hashes(&self, transaction: &UnsealedTransactionV1, blob_hashes: &BlobHashes) -> bool {
        self.verify_message(Self::create_message_v1_with_blob_hashes(transaction, blob_hashes))
    }

    /// Verifies this seal against an already-derived signing message.
    pub fn verify_message(&self, message: [u8; 64]) -> bool {
        let Ok(public_key) = self.public_key.try_from_byte_type() else {
            return false;
        };
        let Ok(signature) = RistrettoSchnorr::convert_from_byte_type(&self.signature) else {
            return false;
        };
        signature.verify(&public_key, message)
    }

    pub fn signature(&self) -> &SchnorrSignatureBytes {
        &self.signature
    }

    pub fn public_key(&self) -> &RistrettoPublicKeyBytes {
        &self.public_key
    }

    pub fn to_ristretto_public_key(&self) -> Result<RistrettoPublicKey, tari_utilities::ByteArrayError> {
        RistrettoPublicKey::from_canonical_bytes(self.public_key.as_bytes())
    }

    pub fn create_message(transaction: &UnsealedTransaction) -> [u8; 64] {
        match transaction {
            UnsealedTransaction::V1(t) => Self::create_message_v1(t),
        }
    }

    pub fn create_message_v1(transaction: &UnsealedTransactionV1) -> [u8; 64] {
        let blob_hashes = transaction.unsigned_transaction().blobs.hashes();
        Self::create_message_v1_with_blob_hashes(transaction, &blob_hashes)
    }

    /// Seal message for a transaction whose blob commitments have already been derived. Produces
    /// the same digest as [`Self::create_message_v1`] given commitments over the same blobs.
    pub fn create_message_v1_with_blob_hashes(
        transaction: &UnsealedTransactionV1,
        blob_hashes: &BlobHashes,
    ) -> [u8; 64] {
        Self::create_message_v1_inner(
            transaction.schema_version(),
            TransactionSignatureFields::from(transaction.unsigned_transaction()),
            blob_hashes,
            transaction.signatures(),
        )
    }

    /// The ordered borsh segments chained into the seal ("SealSignature") message digest.
    ///
    /// Single source of truth for streaming a seal signing request to a hardware signer: the
    /// concatenation of the returned segment bytes is byte-identical to what
    /// [`Self::create_message_v1`] feeds into the hasher (after the domain-separation preamble for
    /// label `"SealSignature"`). Keep this in lock-step with `create_message_v1_inner`.
    pub fn signing_preimage_v1(transaction: &UnsealedTransactionV1) -> Vec<PreimageSegment> {
        let unsigned = transaction.unsigned_transaction();
        let blob_hashes = unsigned.blobs.hashes();
        vec![
            preimage_segment(PreimageField::SchemaVersion, &transaction.schema_version()),
            preimage_segment(PreimageField::Network, &unsigned.network),
            preimage_segment(PreimageField::FeeInstructions, unsigned.fee_instructions.as_slice()),
            preimage_segment(PreimageField::Instructions, unsigned.instructions.as_slice()),
            preimage_segment(PreimageField::Inputs, &unsigned.inputs),
            preimage_segment(PreimageField::MinEpoch, &unsigned.min_epoch),
            preimage_segment(PreimageField::MaxEpoch, &unsigned.max_epoch),
            preimage_segment(
                PreimageField::IsSealSignerAuthorized,
                &unsigned.is_seal_signer_authorized,
            ),
            preimage_segment(PreimageField::DryRun, &unsigned.dry_run),
            preimage_segment(PreimageField::Nonce, &unsigned.nonce),
            preimage_segment(PreimageField::BlobHashes, &blob_hashes),
            preimage_segment(PreimageField::Signatures, transaction.signatures()),
        ]
    }

    /// Pruned-form seal message. Uses the stored `BlobHashes` and produces the same digest as
    /// the equivalent full form would.
    pub fn create_message_v1_pruned(transaction: &PrunedUnsealedTransactionV1) -> [u8; 64] {
        Self::create_message_v1_inner(
            transaction.schema_version(),
            TransactionSignatureFields::from(transaction.unsigned_transaction()),
            transaction.blob_hashes(),
            transaction.signatures(),
        )
    }

    fn create_message_v1_inner(
        schema_version: u16,
        fields: TransactionSignatureFields<'_>,
        blob_hashes: &BlobHashes,
        signatures: &[TransactionSignature],
    ) -> [u8; 64] {
        // Project explicitly so blob bytes never enter the digest — only their commitments do.
        transaction_hasher_v1("SealSignature")
            .chain(&schema_version)
            .chain(&fields)
            .chain(blob_hashes)
            .chain(signatures)
            .result()
    }

    pub fn verify_v1_pruned(&self, transaction: &PrunedUnsealedTransactionV1) -> bool {
        self.verify_message(Self::create_message_v1_pruned(transaction))
    }
}

impl From<SignatureOutput> for TransactionSealSignature {
    fn from(output: SignatureOutput) -> Self {
        Self {
            public_key: output.public_key.to_byte_type(),
            signature: output.signature.to_byte_type(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, borsh::BorshSerialize, minicbor::Encode, minicbor::Decode, minicbor::CborLen)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TransactionSignature {
    #[n(0)]
    public_key: RistrettoPublicKeyBytes,
    #[n(1)]
    signature: SchnorrSignatureBytes,
}

impl TransactionSignature {
    pub fn new(public_key: RistrettoPublicKeyBytes, signature: SchnorrSignatureBytes) -> Self {
        Self { public_key, signature }
    }

    pub fn sign(
        secret_key: &RistrettoSecretKey,
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &UnsignedTransaction,
    ) -> Self {
        match transaction {
            UnsignedTransaction::V1(v1) => Self::sign_v1(secret_key, seal_signer, v1),
        }
    }

    pub fn sign_v1(
        secret_key: &RistrettoSecretKey,
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &UnsignedTransactionV1,
    ) -> Self {
        let public_key = RistrettoPublicKey::from_secret_key(secret_key);
        let message = Self::create_message_v1(seal_signer, transaction);

        Self {
            signature: RistrettoSchnorr::sign(secret_key, message, &mut rand::rng())
                .expect("sign is infallible with Ristretto keys")
                .to_byte_type(),
            public_key: public_key.to_byte_type(),
        }
    }

    pub fn verify_v1(&self, seal_signer: &RistrettoPublicKeyBytes, transaction: &UnsignedTransactionV1) -> bool {
        self.verify_message(Self::create_message_v1(seal_signer, transaction))
    }

    /// Verifies this signature against an already-derived signing message.
    ///
    /// The message is identical for every signature over the same transaction, and deriving it
    /// hashes the entire body — including a commitment over every blob's bytes. A caller verifying
    /// more than one signature MUST derive the message once and use this: deriving it per signature
    /// makes verification cost `O(signatures × body bytes)`, which an attacker controls on both
    /// factors within a single transaction.
    pub fn verify_message(&self, message: [u8; 64]) -> bool {
        let Ok(public_key) = self.public_key.try_from_byte_type() else {
            return false;
        };
        let Ok(signature) = RistrettoSchnorr::convert_from_byte_type(&self.signature) else {
            return false;
        };
        signature.verify(&public_key, message)
    }

    pub fn signature(&self) -> &SchnorrSignatureBytes {
        &self.signature
    }

    pub fn public_key(&self) -> &RistrettoPublicKeyBytes {
        &self.public_key
    }

    pub fn create_message(seal_signer: &RistrettoPublicKeyBytes, transaction: &UnsignedTransaction) -> [u8; 64] {
        match transaction {
            UnsignedTransaction::V1(v1) => Self::create_message_v1(seal_signer, v1),
        }
    }

    pub fn create_message_v1(seal_signer: &RistrettoPublicKeyBytes, transaction: &UnsignedTransactionV1) -> [u8; 64] {
        let blob_hashes = transaction.blobs.hashes();
        Self::create_message_v1_with_blob_hashes(seal_signer, transaction, &blob_hashes)
    }

    /// Authorization message for a transaction whose blob commitments have already been derived.
    /// Produces the same digest as [`Self::create_message_v1`] given commitments over the same
    /// blobs.
    pub fn create_message_v1_with_blob_hashes(
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &UnsignedTransactionV1,
        blob_hashes: &BlobHashes,
    ) -> [u8; 64] {
        Self::create_message_v1_inner(
            seal_signer,
            transaction.schema_version(),
            TransactionSignatureFields::from(transaction),
            blob_hashes,
        )
    }

    /// The ordered borsh segments chained into the authorization ("Signature") message digest.
    ///
    /// This is the single source of truth for streaming an authorization signing request to a
    /// hardware signer: the concatenation of the returned segment bytes is byte-identical to what
    /// [`Self::create_message_v1`] feeds into the hasher (after the domain-separation preamble for
    /// label `"Signature"`). Keep this in lock-step with `create_message_v1_inner`.
    pub fn signing_preimage_v1(
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &UnsignedTransactionV1,
    ) -> Vec<PreimageSegment> {
        let blob_hashes = transaction.blobs.hashes();
        vec![
            preimage_segment(PreimageField::SchemaVersion, &transaction.schema_version()),
            preimage_segment(PreimageField::SealSigner, seal_signer),
            preimage_segment(PreimageField::Network, &transaction.network),
            preimage_segment(PreimageField::FeeInstructions, transaction.fee_instructions.as_slice()),
            preimage_segment(PreimageField::Instructions, transaction.instructions.as_slice()),
            preimage_segment(PreimageField::Inputs, &transaction.inputs),
            preimage_segment(PreimageField::MinEpoch, &transaction.min_epoch),
            preimage_segment(PreimageField::MaxEpoch, &transaction.max_epoch),
            preimage_segment(
                PreimageField::IsSealSignerAuthorized,
                &transaction.is_seal_signer_authorized,
            ),
            preimage_segment(PreimageField::DryRun, &transaction.dry_run),
            preimage_segment(PreimageField::Nonce, &transaction.nonce),
            preimage_segment(PreimageField::BlobHashes, &blob_hashes),
        ]
    }

    /// Pruned-form extra-signer message. Uses the stored `BlobHashes`.
    pub fn create_message_v1_pruned(
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &PrunedUnsignedTransactionV1,
        blob_hashes: &BlobHashes,
    ) -> [u8; 64] {
        Self::create_message_v1_inner(
            seal_signer,
            transaction.schema_version(),
            TransactionSignatureFields::from(transaction),
            blob_hashes,
        )
    }

    fn create_message_v1_inner(
        seal_signer: &RistrettoPublicKeyBytes,
        schema_version: u16,
        fields: TransactionSignatureFields<'_>,
        blob_hashes: &BlobHashes,
    ) -> [u8; 64] {
        transaction_hasher_v1("Signature")
            .chain(&schema_version)
            .chain(seal_signer)
            .chain(&fields)
            .chain(blob_hashes)
            .result()
    }

    pub fn verify_v1_pruned(
        &self,
        seal_signer: &RistrettoPublicKeyBytes,
        transaction: &PrunedUnsignedTransactionV1,
        blob_hashes: &BlobHashes,
    ) -> bool {
        self.verify_message(Self::create_message_v1_pruned(seal_signer, transaction, blob_hashes))
    }

    /// Verifies every signature in `signatures` against `message`, returning `Err(index)` for the
    /// first signature that does not verify.
    ///
    /// Equivalent to calling [`Self::verify_message`] on each in turn, but folds the checks into a
    /// single multiscalar multiplication (see [`Self::verify_batch`]). The individual checks still
    /// run when the batch rejects the set, since they are what identifies the signature at fault.
    pub fn verify_all_against_message(signatures: &[Self], message: [u8; 64]) -> Result<(), usize> {
        if !signatures.is_empty() && Self::verify_batch(signatures, &message) {
            return Ok(());
        }
        signatures
            .iter()
            .position(|sig| !sig.verify_message(message))
            .map_or(Ok(()), Err)
    }

    /// True when every signature in `signatures` verifies against `message`.
    ///
    /// Each signature `(Pᵢ, Rᵢ, sᵢ)` is valid exactly when `sᵢ·G - Rᵢ - eᵢ·Pᵢ` is the identity, for
    /// `eᵢ = H(Rᵢ ‖ Pᵢ ‖ message)`. Weighting those n equations by scalars `zᵢ` and summing them
    /// gives one equation over the whole set:
    ///
    /// ```text
    /// Σ zᵢ·Rᵢ + Σ (zᵢ·eᵢ)·Pᵢ - (Σ zᵢ·sᵢ)·G == 0
    /// ```
    ///
    /// which holds whenever every signature is valid, and otherwise only for a `zᵢ` vector that
    /// makes the individual errors cancel. So one multiscalar multiplication over `2n + 1` terms
    /// stands in for n double-base multiplications: only the scalar arithmetic stays linear in n,
    /// while the point arithmetic is shared across the terms, so the cost per signature keeps
    /// falling as the set grows. Measured in `benches/signature_verification.rs` at 18µs per
    /// signature at sixteen and 14µs at a thousand, against ~44µs for the individual checks.
    ///
    /// The weights are derived from the signature set and the message rather than sampled, so every
    /// node reaches the same verdict on the same bytes — a validity predicate a committee votes on
    /// must not depend on local randomness. Soundness then rests on the same Fiat-Shamir argument as
    /// the challenge scalars themselves: the weights are fixed by a hash of everything they weigh,
    /// so a set whose errors cancel under its own weights takes a search over 2^127 candidates to
    /// find.
    ///
    /// The multiplication is variable-time. Every term is public — keys, nonces, signature scalars,
    /// the message, and weights derived from all of them — so it has no secret to leak, and the
    /// signed-digit recoding it allows is worth ~1.5x over the constant-time algorithm here: the
    /// same equation through `RistrettoPublicKey::batch_mul` measured 1.7x against the individual
    /// checks where this measures 2.5x.
    fn verify_batch(signatures: &[Self], message: &[u8; 64]) -> bool {
        let weights = Self::batch_weights(signatures, message);
        let n = signatures.len();

        let mut nonces = Vec::with_capacity(n);
        let mut public_keys = Vec::with_capacity(n);
        let mut challenge_terms = Vec::with_capacity(n);
        let mut signature_sum = Scalar::ZERO;

        for (sig, z) in signatures.iter().zip(&weights) {
            let Ok(public_key) = sig.public_key.try_from_byte_type() else {
                return false;
            };
            let Ok(signature) = RistrettoSchnorr::convert_from_byte_type(&sig.signature) else {
                return false;
            };
            let e = challenge_scalar(signature.get_public_nonce(), &public_key, message);

            signature_sum += z * Scalar::from(signature.get_signature().clone());
            challenge_terms.push(z * e);
            nonces.push(signature.get_public_nonce().point());
            public_keys.push(public_key.point());
        }

        // Terms `[z₀..zₙ₋₁, z₀e₀..zₙ₋₁eₙ₋₁, -Σ zᵢsᵢ]` against `[R₀..Rₙ₋₁, P₀..Pₙ₋₁, G]`, which the
        // multiplication takes as one flat pair of slices.
        let mut points = Vec::with_capacity(2 * n + 1);
        points.extend(nonces);
        points.extend(public_keys);
        points.push(RISTRETTO_BASEPOINT_POINT);

        let mut scalars = Vec::with_capacity(2 * n + 1);
        scalars.extend(weights);
        scalars.extend(challenge_terms);
        scalars.push(-signature_sum);

        RistrettoPoint::vartime_multiscalar_mul(&scalars, &points) == RistrettoPoint::identity()
    }

    /// The batch weight for each signature: `2¹²⁷ ≤ zᵢ < 2¹²⁸`, derived from a commitment to the
    /// whole set and the message it signs.
    ///
    /// Binding every weight to every signature is what stops a set from being assembled against
    /// weights already known: changing any signature redraws all of them. The weights are
    /// deliberately short — the 2^127 search they impose is the security margin, and a full-width
    /// scalar would cost more to multiply for no gain.
    fn batch_weights(signatures: &[Self], message: &[u8; 64]) -> Vec<Scalar> {
        let commitment = transaction_hasher_v1("BatchVerify")
            .chain(message)
            .chain(signatures)
            .result();

        (0..signatures.len())
            .map(|i| {
                let digest = transaction_hasher_v1("BatchVerifyWeight")
                    .chain(&commitment)
                    .chain(&(i as u64))
                    .result();
                let mut bytes = [0u8; 32];
                // Scalars are little-endian, so the low 16 bytes hold the weight. The top bit of the
                // last of them is set so the weight can never be zero, which would drop its
                // signature from the equation entirely.
                bytes[..16].copy_from_slice(&digest[..16]);
                bytes[15] |= 0x80;
                Option::<Scalar>::from(Scalar::from_canonical_bytes(bytes))
                    .expect("a 128-bit value is a canonical scalar")
            })
            .collect()
    }
}

/// The challenge scalar the stock verifier derives internally: the domain-separated `Blake2b<U64>`
/// hash of `(public_nonce, public_key, message)`, wide-reduced.
///
/// Reproduced here because the batch equation needs the scalar itself, which
/// [`RistrettoSchnorr::verify`] does not expose. It must stay byte-identical to what that verifier
/// computes — a divergence would make the batch and the individual checks disagree — so it is built
/// from tari_crypto's own construction rather than an equivalent hash chain.
fn challenge_scalar(public_nonce: &RistrettoPublicKey, public_key: &RistrettoPublicKey, message: &[u8; 64]) -> Scalar {
    let hash =
        RistrettoSchnorr::construct_domain_separated_challenge::<_, Blake2b<U64>>(public_nonce, public_key, message);
    let scalar = RistrettoSecretKey::from_uniform_bytes(hash.as_ref()).expect("Blake2b<U64> yields 64 uniform bytes");
    Scalar::from(scalar)
}

impl From<SignatureOutput> for TransactionSignature {
    fn from(output: SignatureOutput) -> Self {
        Self {
            public_key: output.public_key.to_byte_type(),
            signature: output.signature.to_byte_type(),
        }
    }
}

/// Field-by-field projection of `UnsignedTransactionV1` used in the signing/id hashing domains.
///
/// Notably *does not* include `blobs` — raw blob bytes never enter signing/id digests. Blob
/// commitments are chained separately as a `BlobHashes` after these fields.
#[derive(Debug, Clone, borsh::BorshSerialize)]
pub(crate) struct TransactionSignatureFields<'a> {
    network: u8,
    fee_instructions: &'a [Instruction],
    instructions: &'a [Instruction],
    inputs: &'a IndexSet<SubstateRequirement>,
    min_epoch: Option<Epoch>,
    max_epoch: Epoch,
    is_seal_signer_authorized: bool,
    dry_run: bool,
    nonce: u64,
}

impl<'a> From<&'a UnsignedTransactionV1> for TransactionSignatureFields<'a> {
    fn from(transaction: &'a UnsignedTransactionV1) -> Self {
        Self {
            network: transaction.network,
            fee_instructions: &transaction.fee_instructions,
            instructions: &transaction.instructions,
            inputs: &transaction.inputs,
            min_epoch: transaction.min_epoch,
            max_epoch: transaction.max_epoch,
            is_seal_signer_authorized: transaction.is_seal_signer_authorized,
            dry_run: transaction.dry_run,
            nonce: transaction.nonce,
        }
    }
}

/// Pruned-form projection. Field-by-field identical to the full-form `From` so the borsh
/// encoding (and therefore the digest) is byte-identical.
impl<'a> From<&'a PrunedUnsignedTransactionV1> for TransactionSignatureFields<'a> {
    fn from(transaction: &'a PrunedUnsignedTransactionV1) -> Self {
        Self {
            network: transaction.network,
            fee_instructions: &transaction.fee_instructions,
            instructions: &transaction.instructions,
            inputs: &transaction.inputs,
            min_epoch: transaction.min_epoch,
            max_epoch: transaction.max_epoch,
            is_seal_signer_authorized: transaction.is_seal_signer_authorized,
            dry_run: transaction.dry_run,
            nonce: transaction.nonce,
        }
    }
}

#[cfg(test)]
mod tests {
    use borsh::BorshSerialize;
    use tari_crypto::keys::SecretKey;
    use tari_engine_types::substate::SubstateId;
    use tari_template_lib_types::ComponentAddress;

    use super::*;

    fn sample_seal_signer() -> RistrettoPublicKeyBytes {
        RistrettoPublicKey::from_secret_key(&RistrettoSecretKey::random(&mut rand::rng())).to_byte_type()
    }

    fn sample_unsigned() -> UnsignedTransactionV1 {
        let mut inputs = IndexSet::new();
        inputs.insert(SubstateRequirement::versioned(
            SubstateId::Component(ComponentAddress::from_array([1; 32])),
            1,
        ));
        inputs.insert(SubstateRequirement::versioned(
            SubstateId::Component(ComponentAddress::from_array([2; 32])),
            2,
        ));
        UnsignedTransactionV1 {
            network: 42,
            fee_instructions: vec![Instruction::DropAllProofsInWorkspace],
            instructions: vec![
                Instruction::DropAllProofsInWorkspace,
                Instruction::PutLastInstructionOutputOnWorkspace { key: 7 },
            ],
            inputs,
            min_epoch: Some(Epoch(100)),
            max_epoch: Epoch(200),
            is_seal_signer_authorized: false,
            dry_run: true,
            blobs: crate::Blobs::empty(),
            nonce: 5,
        }
    }

    fn random_signature(tx: &UnsignedTransactionV1, seal_signer: &RistrettoPublicKeyBytes) -> TransactionSignature {
        let sk = RistrettoSecretKey::random(&mut rand::rng());
        TransactionSignature::sign_v1(&sk, seal_signer, tx)
    }

    fn sig_msg(seal_signer: &RistrettoPublicKeyBytes, tx: &UnsignedTransactionV1) -> [u8; 64] {
        TransactionSignature::create_message_v1(seal_signer, tx)
    }

    fn seal_msg(t: &UnsealedTransactionV1) -> [u8; 64] {
        TransactionSealSignature::create_message_v1(t)
    }

    #[test]
    fn signature_message_is_deterministic() {
        let signer = sample_seal_signer();
        let tx = sample_unsigned();
        assert_eq!(sig_msg(&signer, &tx), sig_msg(&signer, &tx));
    }

    /// Every field of the signed message (seal signer + every field of UnsignedTransactionV1) must
    /// influence the digest. A failure here means a tx field has escaped the signing domain and
    /// signatures are malleable with respect to it.
    #[test]
    fn signature_message_binds_all_fields() {
        let signer = sample_seal_signer();
        let base = sample_unsigned();
        let base_msg = sig_msg(&signer, &base);

        // seal_signer context
        let other_signer = sample_seal_signer();
        assert_ne!(sig_msg(&other_signer, &base), base_msg, "seal_signer");

        // network
        let mut tx = base.clone();
        tx.network = tx.network.wrapping_add(1);
        assert_ne!(sig_msg(&signer, &tx), base_msg, "network");

        // fee_instructions: extra / empty
        let mut tx = base.clone();
        tx.fee_instructions.push(Instruction::DropAllProofsInWorkspace);
        assert_ne!(sig_msg(&signer, &tx), base_msg, "fee_instructions (extra)");
        let mut tx = base.clone();
        tx.fee_instructions.clear();
        assert_ne!(sig_msg(&signer, &tx), base_msg, "fee_instructions (empty)");

        // instructions: extra / reordered
        let mut tx = base.clone();
        tx.instructions.push(Instruction::DropAllProofsInWorkspace);
        assert_ne!(sig_msg(&signer, &tx), base_msg, "instructions (extra)");
        let mut tx = base.clone();
        tx.instructions.reverse();
        assert_ne!(sig_msg(&signer, &tx), base_msg, "instructions (reordered)");

        // inputs: extra / reorder / version changed
        let mut tx = base.clone();
        tx.inputs.insert(SubstateRequirement::versioned(
            SubstateId::Component(ComponentAddress::from_array([9; 32])),
            1,
        ));
        assert_ne!(sig_msg(&signer, &tx), base_msg, "inputs (extra)");

        let mut tx = base.clone();
        tx.inputs = tx.inputs.iter().rev().cloned().collect();
        assert_ne!(sig_msg(&signer, &tx), base_msg, "inputs (reordered)");

        let mut tx = base.clone();
        tx.inputs = base
            .inputs
            .iter()
            .map(|i| SubstateRequirement {
                substate_id: i.substate_id.clone(),
                version: i.version.map(|v| v.wrapping_add(1)),
            })
            .collect();
        assert_ne!(sig_msg(&signer, &tx), base_msg, "inputs (version changed)");

        // min_epoch: value change / Some <-> None
        let mut tx = base.clone();
        tx.min_epoch = Some(Epoch(101));
        assert_ne!(sig_msg(&signer, &tx), base_msg, "min_epoch (value)");
        let mut tx = base.clone();
        tx.min_epoch = None;
        assert_ne!(sig_msg(&signer, &tx), base_msg, "min_epoch (None)");

        // max_epoch
        let mut tx = base.clone();
        tx.max_epoch = Epoch(999);
        assert_ne!(sig_msg(&signer, &tx), base_msg, "max_epoch (value)");

        // is_seal_signer_authorized
        let mut tx = base.clone();
        tx.is_seal_signer_authorized = !tx.is_seal_signer_authorized;
        assert_ne!(sig_msg(&signer, &tx), base_msg, "is_seal_signer_authorized");

        // dry_run
        let mut tx = base.clone();
        tx.dry_run = !tx.dry_run;
        assert_ne!(sig_msg(&signer, &tx), base_msg, "dry_run");

        // nonce
        let mut tx = base.clone();
        tx.nonce = tx.nonce.wrapping_add(1);
        assert_ne!(sig_msg(&signer, &tx), base_msg, "nonce");

        // blobs: payload contents must influence the digest via per-blob commitments
        let mut tx = base.clone();
        tx.blobs.push(crate::Blob::from(vec![1, 2, 3])).unwrap();
        assert_ne!(sig_msg(&signer, &tx), base_msg, "blobs (added)");

        // Two transactions with the same blob count but different bytes must differ.
        let mut a = base.clone();
        a.blobs.push(crate::Blob::from(vec![1])).unwrap();
        let mut b = base.clone();
        b.blobs.push(crate::Blob::from(vec![2])).unwrap();
        assert_ne!(sig_msg(&signer, &a), sig_msg(&signer, &b), "blobs (contents)");
    }

    #[test]
    fn signature_is_bound_to_seal_signer_context() {
        let signer_sk = RistrettoSecretKey::random(&mut rand::rng());
        let seal_signer_pk = sample_seal_signer();
        let other_seal_signer_pk = sample_seal_signer();
        let tx = sample_unsigned();

        let sig = TransactionSignature::sign_v1(&signer_sk, &seal_signer_pk, &tx);
        assert!(sig.verify_v1(&seal_signer_pk, &tx));
        assert!(
            !sig.verify_v1(&other_seal_signer_pk, &tx),
            "a signature made under one seal signer must not verify under another",
        );

        let mut mutated = tx.clone();
        mutated.dry_run = !mutated.dry_run;
        assert!(!sig.verify_v1(&seal_signer_pk, &mutated));
    }

    fn unsealed_with(unsigned: UnsignedTransactionV1, sigs: Vec<TransactionSignature>) -> UnsealedTransactionV1 {
        UnsealedTransactionV1::new(unsigned, sigs)
    }

    #[test]
    fn seal_message_is_deterministic() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let sig = random_signature(&unsigned, &seal_signer);
        let a = unsealed_with(unsigned.clone(), vec![sig.clone()]);
        let b = unsealed_with(unsigned, vec![sig]);
        assert_eq!(seal_msg(&a), seal_msg(&b));
    }

    /// Every field of UnsignedTransactionV1 reached via the seal message must influence the digest.
    #[test]
    fn seal_message_binds_all_unsigned_fields() {
        let seal_signer = sample_seal_signer();
        let base_unsigned = sample_unsigned();
        let sigs = vec![random_signature(&base_unsigned, &seal_signer)];
        let base = unsealed_with(base_unsigned.clone(), sigs.clone());
        let base_msg = seal_msg(&base);

        let with_body = |u: UnsignedTransactionV1| unsealed_with(u, sigs.clone());

        // network
        let mut u = base_unsigned.clone();
        u.network = u.network.wrapping_add(1);
        assert_ne!(seal_msg(&with_body(u)), base_msg, "network");

        // fee_instructions
        let mut u = base_unsigned.clone();
        u.fee_instructions.push(Instruction::DropAllProofsInWorkspace);
        assert_ne!(seal_msg(&with_body(u)), base_msg, "fee_instructions");

        // instructions: extra / reordered
        let mut u = base_unsigned.clone();
        u.instructions.push(Instruction::DropAllProofsInWorkspace);
        assert_ne!(seal_msg(&with_body(u)), base_msg, "instructions (extra)");
        let mut u = base_unsigned.clone();
        u.instructions.reverse();
        assert_ne!(seal_msg(&with_body(u)), base_msg, "instructions (reordered)");

        // inputs: extra / reordered / version changed
        let mut u = base_unsigned.clone();
        u.inputs.insert(SubstateRequirement::versioned(
            SubstateId::Component(ComponentAddress::from_array([9; 32])),
            1,
        ));
        assert_ne!(seal_msg(&with_body(u)), base_msg, "inputs (extra)");

        let mut u = base_unsigned.clone();
        u.inputs = u.inputs.iter().rev().cloned().collect();
        assert_ne!(seal_msg(&with_body(u)), base_msg, "inputs (reordered)");

        let mut u = base_unsigned.clone();
        u.inputs = base_unsigned
            .inputs
            .iter()
            .map(|i| SubstateRequirement {
                substate_id: i.substate_id.clone(),
                version: i.version.map(|v| v.wrapping_add(1)),
            })
            .collect();
        assert_ne!(seal_msg(&with_body(u)), base_msg, "inputs (version changed)");

        // min_epoch
        let mut u = base_unsigned.clone();
        u.min_epoch = Some(Epoch(101));
        assert_ne!(seal_msg(&with_body(u)), base_msg, "min_epoch (value)");
        let mut u = base_unsigned.clone();
        u.min_epoch = None;
        assert_ne!(seal_msg(&with_body(u)), base_msg, "min_epoch (None)");

        // max_epoch
        let mut u = base_unsigned.clone();
        u.max_epoch = Epoch(999);
        assert_ne!(seal_msg(&with_body(u)), base_msg, "max_epoch (value)");

        // is_seal_signer_authorized
        let mut u = base_unsigned.clone();
        u.is_seal_signer_authorized = !u.is_seal_signer_authorized;
        assert_ne!(seal_msg(&with_body(u)), base_msg, "is_seal_signer_authorized");

        // dry_run
        let mut u = base_unsigned.clone();
        u.dry_run = !u.dry_run;
        assert_ne!(seal_msg(&with_body(u)), base_msg, "dry_run");

        // nonce
        let mut u = base_unsigned.clone();
        u.nonce = u.nonce.wrapping_add(1);
        assert_ne!(seal_msg(&with_body(u)), base_msg, "nonce");

        // blobs: changes to the payload must alter the seal digest via per-blob commitments
        let mut u = base_unsigned.clone();
        u.blobs.push(crate::Blob::from(vec![9, 9, 9])).unwrap();
        assert_ne!(seal_msg(&with_body(u)), base_msg, "blobs (added)");
    }

    /// The seal signature binds the prior signatures — their presence, content and order must all
    /// affect the seal digest, otherwise a seal could be lifted onto a transaction with forged
    /// signatures.
    #[test]
    fn seal_message_binds_signatures() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();

        let sig1 = random_signature(&unsigned, &seal_signer);
        let sig2 = random_signature(&unsigned, &seal_signer);

        let no_sigs = unsealed_with(unsigned.clone(), vec![]);
        let with_sig1 = unsealed_with(unsigned.clone(), vec![sig1.clone()]);
        let with_sig2 = unsealed_with(unsigned.clone(), vec![sig2.clone()]);
        let ab = unsealed_with(unsigned.clone(), vec![sig1.clone(), sig2.clone()]);
        let ba = unsealed_with(unsigned, vec![sig2, sig1]);

        assert_ne!(seal_msg(&no_sigs), seal_msg(&with_sig1), "count (0 vs 1)");
        assert_ne!(seal_msg(&with_sig1), seal_msg(&with_sig2), "content");
        assert_ne!(seal_msg(&with_sig1), seal_msg(&ab), "count (1 vs 2)");
        assert_ne!(seal_msg(&ab), seal_msg(&ba), "order");
    }

    /// The streamed preimage segments must concatenate to exactly the byte sequence that
    /// `TransactionSignature::create_message_v1_inner` chains into the hasher. If this drifts, a
    /// hardware signer would compute a different digest and produce signatures that fail to verify.
    #[test]
    fn signature_preimage_matches_chain_order() {
        let signer = sample_seal_signer();
        let tx = sample_unsigned();

        let reconstructed: Vec<u8> = TransactionSignature::signing_preimage_v1(&signer, &tx)
            .iter()
            .flat_map(|s| s.bytes.clone())
            .collect();

        // Mirror create_message_v1_inner exactly: schema_version, seal_signer, fields, blob_hashes.
        let mut expected = Vec::new();
        BorshSerialize::serialize(&tx.schema_version(), &mut expected).unwrap();
        BorshSerialize::serialize(&signer, &mut expected).unwrap();
        BorshSerialize::serialize(&TransactionSignatureFields::from(&tx), &mut expected).unwrap();
        BorshSerialize::serialize(&tx.blobs.hashes(), &mut expected).unwrap();

        assert_eq!(reconstructed, expected);
    }

    /// As above, for the seal ("SealSignature") preimage: schema_version, fields, blob_hashes,
    /// signatures.
    #[test]
    fn seal_preimage_matches_chain_order() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let sig = random_signature(&unsigned, &seal_signer);
        let unsealed = unsealed_with(unsigned.clone(), vec![sig]);

        let reconstructed: Vec<u8> = TransactionSealSignature::signing_preimage_v1(&unsealed)
            .iter()
            .flat_map(|s| s.bytes.clone())
            .collect();

        let mut expected = Vec::new();
        BorshSerialize::serialize(&unsealed.schema_version(), &mut expected).unwrap();
        BorshSerialize::serialize(&TransactionSignatureFields::from(&unsigned), &mut expected).unwrap();
        BorshSerialize::serialize(&unsigned.blobs.hashes(), &mut expected).unwrap();
        BorshSerialize::serialize(&unsealed.signatures(), &mut expected).unwrap();

        assert_eq!(reconstructed, expected);
    }

    /// The `_with_blob_hashes` entry points let a caller derive blob commitments once and share
    /// them between the seal and authorization messages. They must produce digests identical to
    /// deriving the commitments inline — otherwise a signature would verify on one path and fail on
    /// the other.
    #[test]
    fn with_blob_hashes_matches_inline_derivation() {
        let seal_signer = sample_seal_signer();
        let mut unsigned = sample_unsigned();
        unsigned.blobs.push(crate::Blob::from(vec![4, 5, 6])).unwrap();
        let sig = random_signature(&unsigned, &seal_signer);
        let unsealed = unsealed_with(unsigned.clone(), vec![sig]);
        let blob_hashes = unsigned.blobs.hashes();

        assert_eq!(
            TransactionSignature::create_message_v1(&seal_signer, &unsigned),
            TransactionSignature::create_message_v1_with_blob_hashes(&seal_signer, &unsigned, &blob_hashes),
            "authorization message",
        );
        assert_eq!(
            TransactionSealSignature::create_message_v1(&unsealed),
            TransactionSealSignature::create_message_v1_with_blob_hashes(&unsealed, &blob_hashes),
            "seal message",
        );
    }

    /// The signing message is derived once for the whole signature set, so every signature must
    /// still be checked against it individually: a fully valid set verifies, and a signature over a
    /// different body fails at any position in the set.
    #[test]
    fn verify_all_signatures_checks_every_signature() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();

        let sigs: Vec<_> = (0..4).map(|_| random_signature(&unsigned, &seal_signer)).collect();
        let all_valid = unsealed_with(unsigned.clone(), sigs.clone());
        assert!(all_valid.verify_all_signatures(&seal_signer));

        let mut other_body = unsigned.clone();
        other_body.dry_run = !other_body.dry_run;
        let foreign = random_signature(&other_body, &seal_signer);

        for i in 0..sigs.len() {
            let mut planted = sigs.clone();
            planted[i] = foreign.clone();
            assert!(
                !unsealed_with(unsigned.clone(), planted).verify_all_signatures(&seal_signer),
                "a signature over a different body at index {i} must fail verification",
            );
        }

        assert!(
            !all_valid.verify_all_signatures(&sample_seal_signer()),
            "the set is bound to the seal signer it was signed under",
        );
    }

    /// Exchanges the `s` scalars of two valid signatures, leaving each key and nonce in place. The
    /// challenge binds only the nonce and the key, so each result is invalid by exactly the
    /// difference of the two scalars — equal and opposite error terms, which is what a cancelling
    /// pair needs.
    fn swap_scalars(a: &TransactionSignature, b: &TransactionSignature) -> Vec<TransactionSignature> {
        let a_inner = RistrettoSchnorr::convert_from_byte_type(a.signature()).unwrap();
        let b_inner = RistrettoSchnorr::convert_from_byte_type(b.signature()).unwrap();
        vec![
            TransactionSignature::new(
                *a.public_key(),
                RistrettoSchnorr::new(a_inner.get_public_nonce().clone(), b_inner.get_signature().clone())
                    .to_byte_type(),
            ),
            TransactionSignature::new(
                *b.public_key(),
                RistrettoSchnorr::new(b_inner.get_public_nonce().clone(), a_inner.get_signature().clone())
                    .to_byte_type(),
            ),
        ]
    }

    /// The batch must accept exactly the sets the individual checks accept, and name the same
    /// signature when it rejects — at a single signature, at the sizes where the multiscalar
    /// multiplication switches algorithm, and above.
    #[test]
    fn batch_verification_agrees_with_individual_checks() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let message = sig_msg(&seal_signer, &unsigned);

        let mut other_body = unsigned.clone();
        other_body.dry_run = !other_body.dry_run;
        let foreign = random_signature(&other_body, &seal_signer);

        for n in [1usize, 2, 3, 4, 8, 33] {
            let sigs: Vec<_> = (0..n).map(|_| random_signature(&unsigned, &seal_signer)).collect();
            assert_eq!(
                TransactionSignature::verify_all_against_message(&sigs, message),
                Ok(()),
                "{n} valid signatures must verify",
            );

            // Every position for the small sets; the ends and the middle for the large one, whose
            // per-index cost is quadratic and adds no coverage the small sets do not already give.
            let planting_positions: Vec<usize> = if n <= 8 {
                (0..n).collect()
            } else {
                vec![0, n / 2, n - 1]
            };
            for i in planting_positions {
                let mut planted = sigs.clone();
                planted[i] = foreign.clone();
                assert_eq!(
                    TransactionSignature::verify_all_against_message(&planted, message),
                    Err(i),
                    "a foreign signature at index {i} of {n} must be rejected and named",
                );
            }
        }
    }

    /// The reason the weights exist. Two invalid signatures whose error terms are `+delta·G` and
    /// `-delta·G` satisfy the summed equation exactly when both errors carry the same coefficient,
    /// so an unweighted batch accepts a pair that neither individual check would. The weights make
    /// the errors cancel only for a `zᵢ` vector the pair would have to be searched for.
    #[test]
    fn batch_verification_rejects_cancelling_errors() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let message = sig_msg(&seal_signer, &unsigned);

        let first = random_signature(&unsigned, &seal_signer);
        let second = random_signature(&unsigned, &seal_signer);
        let pair = swap_scalars(&first, &second);

        // Neither is a valid signature on its own.
        for (i, sig) in pair.iter().enumerate() {
            assert!(!sig.verify_message(message), "offset signature {i} must not verify");
        }

        // The unweighted sum of their verification equations nonetheless balances: sum(sᵢ)·G ==
        // sum(Rᵢ) + sum(eᵢ·Pᵢ). Asserting it is what pins the test to the property under test — if
        // this ever stops holding, the case below no longer exercises the weights at all.
        let mut lhs = Scalar::ZERO;
        let mut rhs = RistrettoPoint::identity();
        for sig in &pair {
            let public_key = sig.public_key().try_from_byte_type().unwrap();
            let inner = RistrettoSchnorr::convert_from_byte_type(sig.signature()).unwrap();
            let e = challenge_scalar(inner.get_public_nonce(), &public_key, &message);
            lhs += Scalar::from(inner.get_signature().clone());
            rhs += inner.get_public_nonce().point() + public_key.point() * e;
        }
        assert_eq!(
            RISTRETTO_BASEPOINT_POINT * lhs,
            rhs,
            "the pair must balance unweighted, or it does not test the weights",
        );

        assert_eq!(
            TransactionSignature::verify_all_against_message(&pair, message),
            Err(0),
            "a pair whose errors cancel unweighted must still be rejected",
        );
    }

    /// The batch must bind each key to its own nonce and scalar. Verifying the same components under
    /// a permutation would mean a signer could authorize with another's key.
    #[test]
    fn batch_verification_rejects_permuted_components() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let message = sig_msg(&seal_signer, &unsigned);

        let first = random_signature(&unsigned, &seal_signer);
        let second = random_signature(&unsigned, &seal_signer);
        let swapped = vec![
            TransactionSignature::new(*second.public_key(), *first.signature()),
            TransactionSignature::new(*first.public_key(), *second.signature()),
        ];

        assert_eq!(
            TransactionSignature::verify_all_against_message(&swapped, message),
            Err(0),
            "signatures verified under each other's keys must be rejected",
        );
    }

    /// Bytes that decode to neither a key nor a signature are rejected rather than panicking, at any
    /// position in the set.
    #[test]
    fn batch_verification_rejects_undecodable_components() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let message = sig_msg(&seal_signer, &unsigned);

        let valid = random_signature(&unsigned, &seal_signer);
        let bad_key = TransactionSignature::new(
            RistrettoPublicKeyBytes::from_bytes(&[0xff; 32]).unwrap(),
            *valid.signature(),
        );
        let bad_signature = TransactionSignature::new(
            *valid.public_key(),
            SchnorrSignatureBytes::from_bytes(&[0xff; 64]).unwrap(),
        );

        for (label, planted) in [
            ("key", vec![bad_key, valid.clone()]),
            ("signature", vec![valid.clone(), bad_signature]),
        ] {
            assert!(
                TransactionSignature::verify_all_against_message(&planted, message).is_err(),
                "an undecodable {label} must be rejected",
            );
        }
    }

    /// Weights must be a function of the whole set, so that no signature can be chosen against a
    /// weight already known, and identical across nodes, so that a committee cannot split on a
    /// verdict.
    #[test]
    fn batch_weights_are_deterministic_and_bound_to_the_whole_set() {
        let seal_signer = sample_seal_signer();
        let unsigned = sample_unsigned();
        let message = sig_msg(&seal_signer, &unsigned);

        let sigs: Vec<_> = (0..4).map(|_| random_signature(&unsigned, &seal_signer)).collect();
        let weights = TransactionSignature::batch_weights(&sigs, &message);
        assert_eq!(
            weights,
            TransactionSignature::batch_weights(&sigs, &message),
            "weights must be reproducible",
        );
        assert!(
            weights.iter().all(|z| z != &Scalar::ZERO),
            "a zero weight would drop its signature from the equation",
        );

        let mut replaced = sigs.clone();
        replaced[3] = random_signature(&unsigned, &seal_signer);
        let redrawn = TransactionSignature::batch_weights(&replaced, &message);
        assert!(
            weights.iter().zip(&redrawn).all(|(before, after)| before != after),
            "replacing one signature must redraw every weight",
        );

        let other_message = sig_msg(&sample_seal_signer(), &unsigned);
        assert!(
            weights
                .iter()
                .zip(&TransactionSignature::batch_weights(&sigs, &other_message))
                .all(|(before, after)| before != after),
            "the weights must be bound to the message the set signs",
        );
    }

    #[test]
    fn seal_signature_roundtrip() {
        let sealer_sk = RistrettoSecretKey::random(&mut rand::rng());
        let seal_signer_pk: RistrettoPublicKeyBytes = RistrettoPublicKey::from_secret_key(&sealer_sk).to_byte_type();

        let unsigned = sample_unsigned();
        let sig = random_signature(&unsigned, &seal_signer_pk);
        let t = unsealed_with(unsigned, vec![sig]);

        let seal = TransactionSealSignature::sign_v1(&sealer_sk, &t);
        assert!(seal.verify_v1(&t));

        // Mutating a body field breaks the seal.
        let mut mutated_inner = t.unsigned_transaction().clone();
        mutated_inner.dry_run = !mutated_inner.dry_run;
        let mutated = UnsealedTransactionV1::new(mutated_inner, t.signatures().to_vec());
        assert!(!seal.verify_v1(&mutated));

        // Mutating signatures also breaks the seal.
        let extra_sig = random_signature(t.unsigned_transaction(), &seal_signer_pk);
        let mut sigs = t.signatures().to_vec();
        sigs.push(extra_sig);
        let mutated = UnsealedTransactionV1::new(t.unsigned_transaction().clone(), sigs);
        assert!(!seal.verify_v1(&mutated));
    }
}
