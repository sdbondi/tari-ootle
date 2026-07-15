//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::str::FromStr;

use time::{OffsetDateTime, PrimitiveDateTime};

/// The persisted state of a transaction request.
///
/// `Expired` is deliberately absent: expiry is a function of `expires_at` and
/// the current time, derived by [`TransactionRequestModel::effective_status`]
/// on read. Storing it would need a reaper to write it, and a reaper racing
/// the approve path is exactly the bug that "derive it" avoids.
// NOT exported to TypeScript: this is the *stored* status and has no `Expired`
// variant, because expiry is derived on read. `EffectiveStatus` is the wire
// type -- a UI reading this one could not represent an expired request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TransactionRequestStatus {
    /// Created, awaiting a principal holding `transaction_requests:approve`.
    Pending,
    /// Approved and not yet submitted. The approval commits to the request's
    /// `transaction_hash`.
    Approved,
    /// Refused by an approver. Terminal; the request's locks are released.
    Rejected,
    /// Sealed and handed to the transaction service. Terminal.
    Submitted,
}

impl TransactionRequestStatus {
    pub fn as_key_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Approved => "Approved",
            Self::Rejected => "Rejected",
            Self::Submitted => "Submitted",
        }
    }
}

impl FromStr for TransactionRequestStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Pending" => Ok(Self::Pending),
            "Approved" => Ok(Self::Approved),
            "Rejected" => Ok(Self::Rejected),
            "Submitted" => Ok(Self::Submitted),
            _ => Err(()),
        }
    }
}

/// What a caller sees for a request: its stored status, or `Expired` when the
/// approval window has closed on a request that never reached a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "wallet-types/"))]
pub enum EffectiveStatus {
    Pending,
    Approved,
    Rejected,
    Submitted,
    /// `expires_at` has passed while the request was still Pending or
    /// Approved. Nothing writes this; it is derived on read.
    Expired,
}

#[derive(Debug, Clone)]
pub struct TransactionRequestModel {
    pub id: i32,
    pub request_id: String,
    /// Canonical CBOR of the frozen `UnsignedTransaction`. `transaction_hash`
    /// is the hash an approval commits to.
    pub unsigned_transaction: Vec<u8>,
    pub transaction_hash: String,
    pub seal_signer: String,
    pub other_signers: String,
    pub lock_ids: String,
    /// Admin-assigned name of the API key that created this request, or `None`
    /// for a wallet session. Display and audit only.
    pub requested_by: Option<String>,
    pub status: TransactionRequestStatus,
    pub transaction_id: Option<String>,
    pub expires_at: PrimitiveDateTime,
    pub approved_at: Option<PrimitiveDateTime>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

impl TransactionRequestModel {
    /// True once `expires_at` has passed, regardless of stored status.
    pub fn is_past_expiry(&self, now: PrimitiveDateTime) -> bool {
        now > self.expires_at
    }

    /// The status a caller sees. A request that reached a terminal state keeps
    /// it — an approved-and-submitted request does not become Expired just
    /// because its window later closed.
    pub fn effective_status(&self, now: PrimitiveDateTime) -> EffectiveStatus {
        match self.status {
            TransactionRequestStatus::Rejected => EffectiveStatus::Rejected,
            TransactionRequestStatus::Submitted => EffectiveStatus::Submitted,
            TransactionRequestStatus::Pending if self.is_past_expiry(now) => EffectiveStatus::Expired,
            TransactionRequestStatus::Approved if self.is_past_expiry(now) => EffectiveStatus::Expired,
            TransactionRequestStatus::Pending => EffectiveStatus::Pending,
            TransactionRequestStatus::Approved => EffectiveStatus::Approved,
        }
    }

    pub fn effective_status_now(&self) -> EffectiveStatus {
        let now = OffsetDateTime::now_utc();
        self.effective_status(PrimitiveDateTime::new(now.date(), now.time()))
    }
}

#[cfg(test)]
mod tests {
    use time::{Date, Month, Time};

    use super::*;

    fn at(day: u8) -> PrimitiveDateTime {
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::July, day).unwrap(),
            Time::MIDNIGHT,
        )
    }

    fn request(status: TransactionRequestStatus) -> TransactionRequestModel {
        TransactionRequestModel {
            id: 1,
            request_id: "req-1".to_string(),
            unsigned_transaction: vec![],
            transaction_hash: String::new(),
            seal_signer: String::new(),
            other_signers: "[]".to_string(),
            lock_ids: "[]".to_string(),
            requested_by: None,
            status,
            transaction_id: None,
            // The approval window closes on the 15th.
            expires_at: at(15),
            approved_at: None,
            created_at: at(14),
            updated_at: at(14),
        }
    }

    #[test]
    fn inside_the_window_the_stored_status_stands() {
        assert_eq!(
            request(TransactionRequestStatus::Pending).effective_status(at(14)),
            EffectiveStatus::Pending
        );
        assert_eq!(
            request(TransactionRequestStatus::Approved).effective_status(at(14)),
            EffectiveStatus::Approved
        );
    }

    #[test]
    fn past_the_window_an_unresolved_request_is_expired() {
        // Derived, not stored: nothing writes Expired, so no reaper can race
        // the approve path.
        assert_eq!(
            request(TransactionRequestStatus::Pending).effective_status(at(16)),
            EffectiveStatus::Expired
        );
        assert_eq!(
            request(TransactionRequestStatus::Approved).effective_status(at(16)),
            EffectiveStatus::Expired,
            "approved-but-never-submitted expires too, so submit cannot use a stale approval"
        );
    }

    #[test]
    fn a_terminal_request_keeps_its_status_forever() {
        // A submitted transaction does not become Expired just because the
        // window later closed -- it already happened.
        assert_eq!(
            request(TransactionRequestStatus::Submitted).effective_status(at(16)),
            EffectiveStatus::Submitted
        );
        assert_eq!(
            request(TransactionRequestStatus::Rejected).effective_status(at(16)),
            EffectiveStatus::Rejected
        );
    }
}
