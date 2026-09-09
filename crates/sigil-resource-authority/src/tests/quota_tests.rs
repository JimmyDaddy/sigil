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
    book.release(&prof, &reservation).expect("release");
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

#[test]
fn r71_quota_durable_book_rehydrates_active_reservations() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let reservation = {
        let mut book = QuotaBookV1::open(&path, 200).expect("open durable book");
        assert!(book.is_durable());
        let reservation = book.reserve(&prof, 40, 4).expect("reserve");
        assert_eq!(book.workspace_used_bytes(), 40);
        reservation
    };

    let mut reopened = QuotaBookV1::open(&path, 200).expect("rehydrate durable book");
    assert_eq!(reopened.workspace_used_bytes(), 40);
    reopened.release(&prof, &reservation).expect("release");
    assert_eq!(reopened.workspace_used_bytes(), 0);
    drop(reopened);

    let reopened_again = QuotaBookV1::open(&path, 200).expect("replay release");
    assert_eq!(reopened_again.workspace_used_bytes(), 0);
}

#[test]
fn r71_quota_durable_book_rejects_stale_snapshot_without_lost_update() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut first = QuotaBookV1::open(&path, 200).expect("first writer");
    let mut stale = QuotaBookV1::open(&path, 200).expect("stale writer");

    first
        .reserve_owned("first", &prof, 40, 4)
        .expect("first reservation");
    let error = stale
        .reserve_owned("stale", &prof, 20, 2)
        .expect_err("stale quota snapshot must not overwrite the first reservation");
    assert!(matches!(
        error,
        QuotaErrorV1::Journal(message) if message.contains("precondition mismatch")
    ));
    assert_eq!(stale.workspace_used_bytes(), 0);

    let reopened = QuotaBookV1::open(path, 200).expect("reopen");
    assert_eq!(reopened.workspace_used_bytes(), 40);
    assert_eq!(
        reopened
            .reservation_for_owner("first")
            .expect("first reservation retained")
            .reserved_bytes,
        40
    );
    assert!(reopened.reservation_for_owner("stale").is_none());
}

#[test]
fn r71_quota_durable_book_rejects_tampered_chain() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let book = QuotaBookV1::open(&path, 200).expect("open durable book");
    drop(book);
    let mut bytes = std::fs::read(&path).expect("read journal");
    let marker = b"\"workspace_cap\":200";
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("workspace cap marker");
    bytes[offset + marker.len() - 1] = b'1';
    std::fs::write(&path, bytes).expect("tamper journal");
    let error = QuotaBookV1::open(&path, 200).expect_err("tampered journal must fail closed");
    assert!(matches!(error, QuotaErrorV1::Journal(_)));
}

#[test]
fn r71_quota_durable_book_authorizes_exact_monotonic_cap_migration() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    {
        let mut book = QuotaBookV1::open(&path, 200).expect("open old durable book");
        book.reserve_owned("session-a", &prof, 40, 4)
            .expect("reserve before migration");
    }
    let mut expected_snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("old snapshot"))
            .expect("old snapshot shape");
    expected_snapshot["workspace_cap"] = 300.into();

    let migrated = QuotaBookV1::open_with_previous_cap(&path, 300, 200)
        .expect("authorize exact monotonic migration");
    assert_eq!(migrated.workspace_used_bytes(), 40);
    assert_eq!(
        migrated
            .reservation_for_owner("session-a")
            .expect("active reservation survives migration")
            .reserved_bytes,
        40
    );
    let actual_snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("migrated snapshot"))
            .expect("migrated snapshot shape");
    assert_eq!(actual_snapshot, expected_snapshot);
    drop(migrated);

    let reopened = QuotaBookV1::open(&path, 300).expect("migration persists current cap");
    assert_eq!(reopened.workspace_used_bytes(), 40);
}

#[test]
fn r71_quota_durable_book_rejects_unapproved_or_decreasing_cap_migration() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    drop(QuotaBookV1::open(&path, 200).expect("open durable book"));

    let wrong_previous = QuotaBookV1::open_with_previous_cap(&path, 300, 199)
        .expect_err("wrong previous cap must fail closed");
    assert!(matches!(wrong_previous, QuotaErrorV1::Journal(_)));

    let decreasing = QuotaBookV1::open_with_previous_cap(&path, 100, 200)
        .expect_err("decreasing cap must fail closed");
    assert!(matches!(decreasing, QuotaErrorV1::Journal(_)));

    let strict_drift =
        QuotaBookV1::open(&path, 300).expect_err("strict open must reject undeclared cap drift");
    assert!(matches!(strict_drift, QuotaErrorV1::Journal(_)));
}

#[test]
fn r71_quota_cap_migration_rejects_history_exceeding_previous_cap() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    {
        let mut book = QuotaBookV1::open(&path, 300).expect("larger-cap journal");
        let profile = profile(400, 10);
        let reservation = book
            .reserve_owned("settled-owner", &profile, 250, 1)
            .expect("reservation valid under original larger cap");
        book.release(&profile, &reservation)
            .expect("settle historical reservation");
    }
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal bytes"))
            .expect("journal shape");
    snapshot["workspace_cap"] = 200.into();
    let tampered = serde_json::to_vec(&snapshot).expect("tampered snapshot");
    std::fs::write(&path, &tampered).expect("claim smaller predecessor cap");

    let error = QuotaBookV1::open_with_previous_cap(&path, 300, 200)
        .expect_err("migration cannot legitimize a peak above the predecessor cap");
    assert!(matches!(
        error,
        QuotaErrorV1::Journal(message) if message == "quota journal replay exceeds a declared cap"
    ));
    assert_eq!(std::fs::read(&path).expect("unchanged journal"), tampered);
}

#[test]
fn r71_quota_owner_reconcile_replays_and_settles() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 100);
    {
        let mut book = QuotaBookV1::open(&path, 200).expect("open durable book");
        book.reserve_owned("session-a", &prof, 20, 2)
            .expect("reserve owner");
    }

    let mut reopened = QuotaBookV1::open(&path, 200).expect("replay owner");
    assert_eq!(
        reopened
            .reservation_for_owner("session-a")
            .expect("active owner")
            .reserved_bytes,
        20
    );
    reopened
        .reconcile_owned("session-a", &prof, 30, 3)
        .expect("reconcile owner");
    assert_eq!(reopened.workspace_used_bytes(), 30);
    reopened.release_owner("session-a").expect("settle owner");
    assert_eq!(reopened.workspace_used_bytes(), 0);
    drop(reopened);

    let reopened_again = QuotaBookV1::open(&path, 200).expect("replay settlement");
    assert!(reopened_again.reservation_for_owner("session-a").is_none());
    assert_eq!(reopened_again.workspace_used_bytes(), 0);
}

#[test]
fn r71_quota_release_unknown_epoch_is_idempotent() {
    let mut book = QuotaBookV1::new(100);
    let prof = profile(100, 10);
    book.release(
        &prof,
        &QuotaReservationV1 {
            reservation_epoch: 99,
            reserved_bytes: 90,
            reserved_entries: 9,
        },
    )
    .expect("unknown release is idempotent");
    assert_eq!(book.workspace_used_bytes(), 0);
}

#[test]
fn r71_quota_adjustment_is_one_durable_record_with_no_release_gap() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut book = QuotaBookV1::open(&path, 200).expect("durable quota");
    let previous = book
        .reserve_owned("capture", &prof, 20, 1)
        .expect("reserve");
    let replacement = book
        .reconcile_owned("capture", &prof, 40, 2)
        .expect("adjust");
    let snapshot: QuotaJournalSnapshotV1 =
        serde_json::from_slice(&std::fs::read(&path).expect("journal")).expect("snapshot");
    assert_eq!(snapshot.records.len(), 2);
    assert!(matches!(
        &snapshot.records[1].event,
        QuotaJournalEventV1::Adjusted {
            previous_reservation_epoch,
            previous_bytes: 20,
            previous_entries: 1,
            reserved_bytes: 40,
            reserved_entries: 2,
            ..
        } if *previous_reservation_epoch == previous.reservation_epoch
    ));
    let before = std::fs::read(&path).expect("before no-op");
    assert_eq!(
        book.reconcile_owned("capture", &prof, 40, 2)
            .expect("no-op"),
        replacement
    );
    assert_eq!(std::fs::read(&path).expect("after no-op"), before);
    let reopened = QuotaBookV1::open(&path, 200).expect("replay adjustment");
    assert_eq!(reopened.reservation_for_owner("capture"), Some(replacement));
    assert_eq!(reopened.workspace_used_bytes(), 40);
}

#[test]
fn r71_quota_rejected_adjustment_preserves_reservation_memory_and_disk() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut book = QuotaBookV1::open(&path, 80).expect("durable quota");
    let previous = book
        .reserve_owned("capture", &prof, 20, 2)
        .expect("capture");
    book.reserve_owned("other", &prof, 30, 3)
        .expect("other owner");
    let before = std::fs::read(&path).expect("before failures");
    assert!(matches!(
        book.reconcile_owned("capture", &prof, 71, 2),
        Err(QuotaErrorV1::ReservationExceeded { .. })
    ));
    assert!(matches!(
        book.reconcile_owned("capture", &prof, 20, 8),
        Err(QuotaErrorV1::EntryExceeded { .. })
    ));
    assert!(matches!(
        book.reconcile_owned("capture", &prof, 51, 2),
        Err(QuotaErrorV1::WorkspaceOvercommit { .. })
    ));
    assert_eq!(book.reservation_for_owner("capture"), Some(previous));
    assert_eq!(book.workspace_used_bytes(), 50);
    assert_eq!(std::fs::read(&path).expect("unchanged journal"), before);
    let reopened = QuotaBookV1::open(&path, 80).expect("replay old reservation");
    assert_eq!(reopened.reservation_for_owner("capture"), Some(previous));
    book.release(&prof, &previous)
        .expect("old receipt still settles");
    assert_eq!(book.workspace_used_bytes(), 30);
}

#[test]
fn r71_quota_capacity_clamps_spare_to_available_class_and_workspace_bytes() {
    let mut book = QuotaBookV1::new(80);
    let prof = profile(100, 10);
    let first = book
        .reserve_owned_capacity("first", &prof, 20, 35, 1)
        .expect("first");
    assert_eq!(first.reserved_bytes, 35);
    let second = book
        .reserve_owned_capacity("second", &prof, 30, 100, 1)
        .expect("second");
    assert_eq!(second.reserved_bytes, 45);
    assert_eq!(book.workspace_used_bytes(), 80);
    assert!(matches!(
        book.reserve_owned_capacity("first", &prof, 36, 100, 1),
        Err(QuotaErrorV1::WorkspaceOvercommit { .. })
    ));
    assert_eq!(book.reservation_for_owner("first"), Some(first));
    book.release_owner("second").expect("settle second");
    assert_eq!(
        book.reserve_owned_capacity("first", &prof, 36, 100, 1)
            .expect("grow")
            .reserved_bytes,
        80
    );

    let mut class_limited = QuotaBookV1::new(200);
    class_limited
        .reserve_owned("other", &prof, 70, 1)
        .expect("class peer");
    assert_eq!(
        class_limited
            .reserve_owned_capacity("capture", &prof, 10, 80, 1)
            .expect("class clamp")
            .reserved_bytes,
        30
    );
}

#[test]
fn r71_quota_capacity_rejects_invalid_range_and_integer_overcommit() {
    let mut book = QuotaBookV1::new(u64::MAX);
    let prof = profile(u64::MAX, u64::MAX);
    assert_eq!(
        book.reserve_owned_capacity("capture", &prof, 2, 1, 1),
        Err(QuotaErrorV1::InvalidCapacityRange)
    );
    book.reserve_owned("other", &prof, u64::MAX, 1)
        .expect("full capacity");
    assert!(matches!(
        book.reserve_owned_capacity("capture", &prof, 1, u64::MAX, 1),
        Err(QuotaErrorV1::ReservationExceeded { .. })
    ));
    assert!(book.reservation_for_owner("capture").is_none());
}

#[test]
fn r71_quota_stale_adjustment_cannot_overwrite_current_owner_capacity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut first = QuotaBookV1::open(&path, 200).expect("first writer");
    let previous = first
        .reserve_owned("capture", &prof, 20, 1)
        .expect("reserve");
    let mut stale = QuotaBookV1::open(&path, 200).expect("stale writer");
    let current = first
        .reconcile_owned("capture", &prof, 40, 1)
        .expect("first growth");
    assert!(matches!(stale.reconcile_owned("capture", &prof, 30, 1),
        Err(QuotaErrorV1::Journal(message)) if message.contains("precondition mismatch")));
    assert_eq!(stale.reservation_for_owner("capture"), Some(previous));
    assert_eq!(
        QuotaBookV1::open(&path, 200)
            .expect("replay")
            .reservation_for_owner("capture"),
        Some(current)
    );
}

#[test]
fn r71_quota_adjustment_replay_checks_exact_predecessor_even_with_valid_hash() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut book = QuotaBookV1::open(&path, 200).expect("quota");
    book.reserve_owned("capture", &prof, 20, 1)
        .expect("reserve");
    book.reconcile_owned("capture", &prof, 40, 1).expect("grow");
    drop(book);
    let mut snapshot: QuotaJournalSnapshotV1 =
        serde_json::from_slice(&std::fs::read(&path).expect("journal")).expect("snapshot");
    let adjustment = snapshot.records.last_mut().expect("adjustment");
    let QuotaJournalEventV1::Adjusted { previous_bytes, .. } = &mut adjustment.event else {
        panic!("expected adjustment event");
    };
    *previous_bytes += 1;
    adjustment.record_hash = quota_record_hash(
        adjustment.sequence,
        adjustment.previous_hash,
        &adjustment.event,
    )
    .expect("rehash");
    std::fs::write(
        &path,
        serde_json::to_vec(&snapshot).expect("tampered snapshot"),
    )
    .expect("tamper");
    assert!(matches!(QuotaBookV1::open(&path, 200),
        Err(QuotaErrorV1::Journal(message)) if message.contains("does not match its predecessor")));
}

#[test]
fn r71_quota_adjustment_file_sync_failure_keeps_previous_reservation_retryable() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut book = QuotaBookV1::open(&path, 200).expect("quota");
    let previous = book
        .reserve_owned("capture", &prof, 20, 1)
        .expect("reserve");
    let before = std::fs::read(&path).expect("before failure");
    book.journal
        .as_ref()
        .expect("journal")
        .persistence_failure
        .set(Some(QuotaPersistenceFailurePoint::FileSync));
    assert!(book.reconcile_owned("capture", &prof, 40, 1).is_err());
    assert_eq!(book.reservation_for_owner("capture"), Some(previous));
    assert_eq!(std::fs::read(&path).expect("unchanged journal"), before);
    book.reconcile_owned("capture", &prof, 40, 1)
        .expect("retry growth");
    assert_eq!(
        QuotaBookV1::open(&path, 200)
            .expect("reopen")
            .workspace_used_bytes(),
        40
    );
}

#[test]
fn r71_quota_uncertain_adjustment_poisoning_requires_reopen_before_reuse() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("quota.json");
    let prof = profile(100, 10);
    let mut book = QuotaBookV1::open(&path, 200).expect("quota");
    let previous = book
        .reserve_owned("capture", &prof, 40, 1)
        .expect("reserve");
    book.journal
        .as_ref()
        .expect("journal")
        .persistence_failure
        .set(Some(QuotaPersistenceFailurePoint::DirectorySync));
    assert!(matches!(book.reconcile_owned("capture", &prof, 20, 1),
        Err(QuotaErrorV1::Journal(message)) if message.contains("uncertain")));
    assert_eq!(book.reservation_for_owner("capture"), Some(previous));
    assert!(matches!(book.reconcile_owned("capture", &prof, 40, 1),
        Err(QuotaErrorV1::Journal(message)) if message.contains("poisoned")));
    assert!(book.reserve_owned("other", &prof, 1, 1).is_err());
    assert!(book.release_owner("capture").is_err());
    let mut reopened = QuotaBookV1::open(&path, 200).expect("reopen installed snapshot");
    assert_eq!(reopened.workspace_used_bytes(), 20);
    reopened
        .reconcile_owned("capture", &prof, 30, 1)
        .expect("recovered growth");
}
