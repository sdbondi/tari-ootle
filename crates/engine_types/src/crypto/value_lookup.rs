//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_template_lib::types::crypto::RistrettoPublicKeyBytes;

/// A reverse lookup from a value point `v·G` to the scalar value `v`.
///
/// This is the core of ElGamal viewable-balance decryption: the ciphertext yields the point `V = E - p·R`
/// (`== v·G` for the real value `v`), and the implementation recovers `v`. Implementations differ in how they
/// cover the value space — a precomputed file (fast binary search) or on-the-fly point generation (slow).
pub trait ValueLookup {
    type Error: std::error::Error;

    /// Returns the value `v` such that `v·G == point`, or `None` if it is outside this lookup's coverage.
    fn lookup(&self, point: &RistrettoPublicKeyBytes) -> Result<Option<u64>, Self::Error>;

    /// Looks up many points at once. The default calls [`lookup`](Self::lookup) per point; implementations
    /// with expensive per-call setup (e.g. a range scan) should override this to amortize the cost.
    fn lookup_many(&self, points: &[RistrettoPublicKeyBytes]) -> Result<Vec<Option<u64>>, Self::Error> {
        points.iter().map(|point| self.lookup(point)).collect()
    }
}
