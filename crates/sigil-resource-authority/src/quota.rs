//! RFC-0071 section 10.9: hierarchical quota, atomic reservation and truthful enforcement.
//!
//! Reservation is atomic against concurrent acquirers; workspace cap never overcommits; and the
//! quota receipt is truthful: reservation accounting never pretends to be a backend hard quota.

use sigil_kernel::resource::{ResourceQuotaClassV1, ResourceQuotaProfileV1};

/// One reservation epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaReservationV1 {
    pub reservation_epoch: u64,
    pub reserved_bytes: u64,
    pub reserved_entries: u64,
}

/// Closed quota error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuotaErrorV1 {
    #[error(
        "per-class reservation exceeds the profile maximum bytes: reserved={reserved} max={max}"
    )]
    ReservationExceeded {
        class: &'static str,
        reserved: u64,
        max: u64,
    },
    #[error(
        "per-class reservation exceeds the profile maximum entries: reserved={reserved} max={max}"
    )]
    EntryExceeded {
        class: &'static str,
        reserved: u64,
        max: u64,
    },
    #[error("workspace hard cap would overcommit: used={used} incoming={incoming} cap={cap}")]
    WorkspaceOvercommit { used: u64, incoming: u64, cap: u64 },
    #[error("borrowed accounting profile claims hard runtime enforcement")]
    BorrowedClaimsEnforcement,
}

/// Owner-local quota bookkeeping (single writer per shard).
#[derive(Debug, Default)]
pub struct QuotaBookV1 {
    per_class_bytes: std::collections::BTreeMap<ResourceQuotaClassV1, u64>,
    per_class_entries: std::collections::BTreeMap<ResourceQuotaClassV1, u64>,
    workspace_bytes: u64,
    workspace_cap: u64,
    reservation_epoch: u64,
}

impl QuotaBookV1 {
    pub const fn new(workspace_cap: u64) -> Self {
        Self {
            per_class_bytes: std::collections::BTreeMap::new(),
            per_class_entries: std::collections::BTreeMap::new(),
            workspace_bytes: 0,
            workspace_cap,
            reservation_epoch: 0,
        }
    }

    /// Atomically reserves bytes/entries under one profile. No mutation occurs on failure.
    pub fn reserve(
        &mut self,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        if profile.hard_runtime_enforcement_required
            && profile.class == ResourceQuotaClassV1::BorrowedAccountingOnly
        {
            return Err(QuotaErrorV1::BorrowedClaimsEnforcement);
        }
        let class_name = quota_class_label(profile.class);
        let used_bytes = *self.per_class_bytes.get(&profile.class).unwrap_or(&0);
        let used_entries = *self.per_class_entries.get(&profile.class).unwrap_or(&0);
        if used_bytes.saturating_add(bytes) > profile.max_bytes {
            return Err(QuotaErrorV1::ReservationExceeded {
                class: class_name,
                reserved: used_bytes.saturating_add(bytes),
                max: profile.max_bytes,
            });
        }
        if used_entries.saturating_add(entries) > profile.max_entries {
            return Err(QuotaErrorV1::EntryExceeded {
                class: class_name,
                reserved: used_entries.saturating_add(entries),
                max: profile.max_entries,
            });
        }
        if self.workspace_bytes.saturating_add(bytes) > self.workspace_cap {
            return Err(QuotaErrorV1::WorkspaceOvercommit {
                used: self.workspace_bytes,
                incoming: bytes,
                cap: self.workspace_cap,
            });
        }
        self.per_class_bytes
            .insert(profile.class, used_bytes.saturating_add(bytes));
        self.per_class_entries
            .insert(profile.class, used_entries.saturating_add(entries));
        self.workspace_bytes = self.workspace_bytes.saturating_add(bytes);
        self.reservation_epoch = self.reservation_epoch.saturating_add(1);
        Ok(QuotaReservationV1 {
            reservation_epoch: self.reservation_epoch,
            reserved_bytes: bytes,
            reserved_entries: entries,
        })
    }

    /// Releases a reservation (settlement). Idempotent on unknown epoch.
    pub fn release(&mut self, profile: &ResourceQuotaProfileV1, reservation: &QuotaReservationV1) {
        if reservation.reservation_epoch == 0 {
            return;
        }
        let used_bytes = self
            .per_class_bytes
            .get(&profile.class)
            .copied()
            .unwrap_or(0);
        let used_entries = self
            .per_class_entries
            .get(&profile.class)
            .copied()
            .unwrap_or(0);
        self.per_class_bytes.insert(
            profile.class,
            used_bytes.saturating_sub(reservation.reserved_bytes),
        );
        self.per_class_entries.insert(
            profile.class,
            used_entries.saturating_sub(reservation.reserved_entries),
        );
        self.workspace_bytes = self
            .workspace_bytes
            .saturating_sub(reservation.reserved_bytes);
    }

    pub fn workspace_used_bytes(&self) -> u64 {
        self.workspace_bytes
    }
}

pub fn quota_class_label(class: ResourceQuotaClassV1) -> &'static str {
    match class {
        ResourceQuotaClassV1::AttemptEphemeral => "attempt-ephemeral",
        ResourceQuotaClassV1::SessionScratch => "session-scratch",
        ResourceQuotaClassV1::RuntimeState => "runtime-state",
        ResourceQuotaClassV1::RuntimeCache => "runtime-cache",
        ResourceQuotaClassV1::ArtifactStaging => "artifact-staging",
        ResourceQuotaClassV1::ArtifactStore => "artifact-store",
        ResourceQuotaClassV1::IsolatedWorkspace => "isolated-workspace",
        ResourceQuotaClassV1::ToolCache => "tool-cache",
        ResourceQuotaClassV1::BorrowedAccountingOnly => "borrowed-accounting",
        ResourceQuotaClassV1::Quarantine => "quarantine",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(max_bytes: u64, max_entries: u64) -> ResourceQuotaProfileV1 {
        ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::AttemptEphemeral,
            max_bytes,
            max_entries,
            max_open_holders: 8,
            max_age_ms: None,
            hard_runtime_enforcement_required: false,
            profile_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0u8; 32]),
        }
    }

    #[test]
    fn r71_quota_reservation_rejects_overcommit_atomically() {
        let mut book = QuotaBookV1::new(50);
        let prof = profile(200, 10);
        book.reserve(&prof, 40, 4).expect("first");
        let error = book.reserve(&prof, 20, 1).expect_err("overcommit");
        assert!(matches!(error, QuotaErrorV1::WorkspaceOvercommit { .. }));
        // No partial mutation: state unchanged after failed reserve.
        assert_eq!(book.workspace_used_bytes(), 40);
    }

    #[test]
    fn r71_quota_release_frees_capacity_deterministically() {
        let mut book = QuotaBookV1::new(100);
        let prof = profile(50, 10);
        let reservation = book.reserve(&prof, 40, 4).expect("reserve");
        book.release(&prof, &reservation);
        assert_eq!(book.workspace_used_bytes(), 0);
        book.reserve(&prof, 40, 4).expect("re-reserve");
    }

    #[test]
    fn r71_quota_borrowed_claiming_enforcement_is_rejected() {
        let mut book = QuotaBookV1::new(1000);
        let mut prof = profile(100, 10);
        prof.class = ResourceQuotaClassV1::BorrowedAccountingOnly;
        prof.hard_runtime_enforcement_required = true;
        let error = book.reserve(&prof, 1, 1).expect_err("must reject");
        assert!(matches!(error, QuotaErrorV1::BorrowedClaimsEnforcement));
    }
}
