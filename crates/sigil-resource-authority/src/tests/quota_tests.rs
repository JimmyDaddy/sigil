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
