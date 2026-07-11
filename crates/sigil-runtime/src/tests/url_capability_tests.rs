use std::{sync::Arc, time::Duration};

use sigil_kernel::{
    SecretString, Session, ToolRestartPolicy, UserUrlCapabilityRegistrar,
    UserUrlCapabilityRegistration, WebUrlCapabilityDescriptor, WebUrlProvenanceKind,
    project_user_message_for_persistence_with_nonce,
};

use super::{
    DEFAULT_URL_CAPABILITY_CAPACITY, DEFAULT_URL_CAPABILITY_TTL, UrlCapabilityLookupError,
    WebUrlCapabilityStore, attach_session_url_capability_store,
};

const SESSION: &str = "session-a";
const SOURCE_1: &str = "src_00000000000000000000000000000001";
const SOURCE_2: &str = "src_00000000000000000000000000000002";
const SOURCE_3: &str = "src_00000000000000000000000000000003";

fn registration(
    source_id: &str,
    durable_entry_id: &str,
    raw_url: &str,
    safe_display_url: &str,
    restart_policy: ToolRestartPolicy,
) -> UserUrlCapabilityRegistration {
    let replayable_canonical_url =
        (restart_policy == ToolRestartPolicy::Replayable).then(|| raw_url.to_owned());
    UserUrlCapabilityRegistration {
        source_id: source_id.to_owned(),
        durable_entry_id: durable_entry_id.to_owned(),
        raw_canonical_url: SecretString::new(raw_url),
        safe_display_url: safe_display_url.to_owned(),
        restart_policy,
        replayable_canonical_url,
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
    }
}

fn stage_and_commit(store: &WebUrlCapabilityStore, source_id: &str, durable_entry_id: &str) {
    store
        .stage(registration(
            source_id,
            durable_entry_id,
            &format!("https://example.com/{durable_entry_id}?token=secret-{source_id}"),
            &format!("https://example.com/{durable_entry_id}?[redacted]"),
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");
    store
        .commit_message(durable_entry_id)
        .expect("message should commit");
}

#[test]
fn url_capability_defaults_match_contract() {
    assert_eq!(DEFAULT_URL_CAPABILITY_TTL, Duration::from_secs(3_600));
    assert_eq!(DEFAULT_URL_CAPABILITY_CAPACITY, 256);
}

#[test]
fn session_attachment_rejects_double_attach_without_replacing_live_store() {
    let mut session = Session::new("provider", "model");
    let first =
        attach_session_url_capability_store(&mut session).expect("first attach should work");
    let source_id = SOURCE_1;
    let session_scope_id = session.session_scope_id().to_owned();
    first
        .stage(registration(
            source_id,
            "message-1",
            "https://example.com/path?token=live-secret",
            "https://example.com/path?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");
    first
        .commit_message("message-1")
        .expect("message should commit");

    let error = attach_session_url_capability_store(&mut session)
        .expect_err("double attach must fail closed instead of replacing live state");
    assert!(error.to_string().contains("already has"));
    assert!(first.resolve(&session_scope_id, source_id).is_ok());
    assert!(session.user_url_capability_registrar().is_some());
}

#[test]
fn url_capability_lookup_errors_have_stable_machine_codes() {
    assert_eq!(UrlCapabilityLookupError::NotFound.code(), "not_found");
    assert_eq!(UrlCapabilityLookupError::Expired.code(), "expired");
    assert_eq!(UrlCapabilityLookupError::Evicted.code(), "evicted");
    assert_eq!(
        UrlCapabilityLookupError::InterruptedOnRestart.code(),
        "sensitive_url_not_replayable"
    );
}

#[test]
fn url_capability_is_invisible_until_commit_and_debug_redacts_raw_url() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let raw_url = "https://example.com/path?token=raw-secret";
    store
        .stage(registration(
            SOURCE_1,
            "message-1",
            raw_url,
            "https://example.com/path?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");

    assert_eq!(store.staged_len(), 1);
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::NotFound)
    );

    store
        .commit_message("message-1")
        .expect("message should commit");
    let capability = store
        .resolve(SESSION, SOURCE_1)
        .expect("capability should resolve");
    assert_eq!(capability.session_scope_id(), SESSION);
    assert_eq!(capability.source_id(), SOURCE_1);
    assert_eq!(capability.durable_entry_id(), "message-1");
    assert_eq!(capability.raw_canonical_url().expose_secret(), raw_url);
    assert_eq!(
        capability.safe_display_url(),
        "https://example.com/path?[redacted]"
    );
    assert_eq!(
        capability.restart_policy(),
        ToolRestartPolicy::InterruptOnRestart
    );
    assert!(!format!("{capability:?}").contains("raw-secret"));
    assert!(!format!("{store:?}").contains("raw-secret"));
}

#[test]
fn url_capability_stage_commit_and_rollback_are_idempotent() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let first = registration(
        SOURCE_1,
        "message-1",
        "https://example.com/path?token=one",
        "https://example.com/path?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
    );
    store.stage(first.clone()).expect("first stage should work");
    store
        .stage(first.clone())
        .expect("repeat stage should work");
    assert_eq!(store.staged_len(), 1);
    store
        .commit_message("message-1")
        .expect("first commit should work");
    store
        .commit_message("message-1")
        .expect("repeat commit should work");
    store
        .stage(first)
        .expect("retry after commit should not duplicate");
    store
        .rollback_message("message-1")
        .expect("rollback after commit should be a no-op");
    assert_eq!(store.active_len(), 1);

    let rolled_back = registration(
        SOURCE_2,
        "message-2",
        "https://example.com/path?token=two",
        "https://example.com/path?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
    );
    store.stage(rolled_back).expect("stage should work");
    store
        .rollback_message("message-2")
        .expect("first rollback should work");
    store
        .rollback_message("message-2")
        .expect("repeat rollback should work");
    store
        .commit_message("message-2")
        .expect("URL-free commit marker should work");
    assert_eq!(
        store.resolve(SESSION, SOURCE_2),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn url_capability_rollback_after_append_failure_leaves_no_live_secret() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    store
        .stage(registration(
            SOURCE_1,
            "message-1",
            "https://example.com/?signature=never-durable",
            "https://example.com/?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");
    store
        .rollback_message("message-1")
        .expect("append failure should roll back");

    assert_eq!(store.staged_len(), 0);
    assert_eq!(store.active_len(), 0);
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn url_capability_rejects_conflicting_or_late_registration() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    store
        .stage(registration(
            SOURCE_1,
            "message-1",
            "https://example.com/?token=one",
            "https://example.com/?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");
    assert!(
        store
            .stage(registration(
                SOURCE_1,
                "message-2",
                "https://example.com/?token=two",
                "https://example.com/?[redacted]",
                ToolRestartPolicy::InterruptOnRestart,
            ))
            .is_err()
    );
    store
        .commit_message("message-1")
        .expect("message should commit");
    assert!(
        store
            .stage(registration(
                SOURCE_2,
                "message-1",
                "https://example.com/?token=late",
                "https://example.com/?[redacted]",
                ToolRestartPolicy::InterruptOnRestart,
            ))
            .is_err()
    );
}

#[test]
fn url_capability_expiry_returns_typed_error() {
    let store =
        WebUrlCapabilityStore::with_limits(SESSION, Duration::ZERO, 2).expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");

    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::Expired)
    );
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::Expired)
    );
    assert_eq!(store.active_len(), 0);
}

#[test]
fn url_capability_capacity_uses_session_local_lru() {
    let store = WebUrlCapabilityStore::with_limits(SESSION, Duration::from_secs(60), 2)
        .expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");
    stage_and_commit(&store, SOURCE_2, "message-2");
    store
        .resolve(SESSION, SOURCE_1)
        .expect("lookup should make source one most recent");
    stage_and_commit(&store, SOURCE_3, "message-3");

    assert_eq!(store.active_len(), 2);
    assert!(store.resolve(SESSION, SOURCE_1).is_ok());
    assert!(store.resolve(SESSION, SOURCE_3).is_ok());
    assert_eq!(
        store.resolve(SESSION, SOURCE_2),
        Err(UrlCapabilityLookupError::Evicted)
    );
}

#[test]
fn url_capability_retry_does_not_resurrect_evicted_source() {
    let store = WebUrlCapabilityStore::with_limits(SESSION, Duration::from_secs(60), 1)
        .expect("store should build");
    let first = registration(
        SOURCE_1,
        "message-1",
        "https://example.com/?token=one",
        "https://example.com/?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
    );
    store.stage(first.clone()).expect("stage should work");
    store
        .commit_message("message-1")
        .expect("commit should work");
    stage_and_commit(&store, SOURCE_2, "message-2");
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::Evicted)
    );

    store
        .stage(first)
        .expect("retry after committed eviction should be idempotent");
    store
        .commit_message("message-1")
        .expect("repeat commit should be idempotent");
    assert_eq!(store.active_len(), 1);
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::Evicted)
    );
}

#[test]
fn full_live_capacity_stage_then_rollback_preserves_existing_capability() {
    let store = WebUrlCapabilityStore::with_limits(SESSION, Duration::from_secs(60), 1)
        .expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");

    store
        .stage(registration(
            SOURCE_2,
            "message-2",
            "https://example.com/message-2?token=provisional",
            "https://example.com/message-2?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("bounded in-flight stage should not evict committed state");
    assert_eq!(store.active_len(), 1);
    assert_eq!(store.staged_len(), 1);
    assert!(store.resolve(SESSION, SOURCE_1).is_ok());
    store
        .rollback_message("message-2")
        .expect("append-failure rollback should remain idempotent");

    assert_eq!(store.active_len(), 1);
    assert!(store.resolve(SESSION, SOURCE_1).is_ok());
    assert_eq!(
        store.resolve(SESSION, SOURCE_2),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn url_capability_cannot_cross_session_scope() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");

    assert_eq!(
        store.resolve("session-b", SOURCE_1),
        Err(UrlCapabilityLookupError::NotFound)
    );
    assert!(store.resolve(SESSION, SOURCE_1).is_ok());
}

#[test]
fn url_capability_restart_interruption_requires_durable_proof() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::NotFound)
    );
    assert_eq!(
        store.resolve_with_durable_descriptor(&WebUrlCapabilityDescriptor {
            session_scope_id: SESSION.to_owned(),
            source_id: SOURCE_1.to_owned(),
            durable_entry_id: "message-1".to_owned(),
            safe_display_url: "https://example.com/?[redacted]".to_owned(),
            restart_policy: ToolRestartPolicy::InterruptOnRestart,
            replayable_canonical_url: None,
            originating_call_id: None,
            provenance: WebUrlProvenanceKind::UserMessage,
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
        }),
        Err(UrlCapabilityLookupError::InterruptedOnRestart)
    );
    assert_eq!(
        store.resolve_with_durable_descriptor(&WebUrlCapabilityDescriptor {
            session_scope_id: "session-b".to_owned(),
            source_id: SOURCE_1.to_owned(),
            durable_entry_id: "message-1".to_owned(),
            safe_display_url: "https://example.com/?[redacted]".to_owned(),
            restart_policy: ToolRestartPolicy::InterruptOnRestart,
            replayable_canonical_url: None,
            originating_call_id: None,
            provenance: WebUrlProvenanceKind::UserMessage,
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
        }),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn sensitive_capability_recovery_reuses_durable_logical_session_scope() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let registration = registration(
        SOURCE_1,
        "message-1",
        "https://example.com/private?signature=secret",
        "https://example.com/private?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
    );
    let descriptor = registration.durable_descriptor(SESSION);
    store
        .stage(registration)
        .expect("registration should stage");
    store
        .commit_message("message-1")
        .expect("message should commit");
    assert!(store.resolve(SESSION, SOURCE_1).is_ok());
    drop(store);

    let recovered_store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let error = recovered_store
        .resolve_with_durable_descriptor(&descriptor)
        .expect_err("sensitive capability must interrupt after restart");
    assert_eq!(error, UrlCapabilityLookupError::InterruptedOnRestart);
    assert_eq!(error.code(), "sensitive_url_not_replayable");

    let other_session = WebUrlCapabilityStore::new("session-b").expect("store should build");
    assert_eq!(
        other_session.resolve_with_durable_descriptor(&descriptor),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn url_capability_tombstone_precedes_restart_interruption() {
    let store =
        WebUrlCapabilityStore::with_limits(SESSION, Duration::ZERO, 1).expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");

    assert_eq!(
        store.resolve_with_durable_descriptor(&WebUrlCapabilityDescriptor {
            session_scope_id: SESSION.to_owned(),
            source_id: SOURCE_1.to_owned(),
            durable_entry_id: "message-1".to_owned(),
            safe_display_url: "https://example.com/?[redacted]".to_owned(),
            restart_policy: ToolRestartPolicy::InterruptOnRestart,
            replayable_canonical_url: None,
            originating_call_id: None,
            provenance: WebUrlProvenanceKind::UserMessage,
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
        }),
        Err(UrlCapabilityLookupError::Expired)
    );
}

#[test]
fn queryless_replayable_url_restores_only_from_explicit_validated_material() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    store
        .stage(registration(
            SOURCE_1,
            "message-1",
            "https://example.com/public",
            "https://example.com/public",
            ToolRestartPolicy::Replayable,
        ))
        .expect("queryless URL should stage");
    store
        .commit_message("message-1")
        .expect("message should commit");
    assert_eq!(
        store
            .resolve(SESSION, SOURCE_1)
            .expect("live replayable capability should resolve")
            .raw_canonical_url()
            .expose_secret(),
        "https://example.com/public"
    );

    let descriptor = WebUrlCapabilityDescriptor {
        session_scope_id: SESSION.to_owned(),
        source_id: SOURCE_1.to_owned(),
        durable_entry_id: "message-1".to_owned(),
        safe_display_url: "https://example.com/public".to_owned(),
        restart_policy: ToolRestartPolicy::Replayable,
        replayable_canonical_url: Some("https://example.com/public".to_owned()),
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
    };
    drop(store);
    let recovered_store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let recovered = recovered_store
        .resolve_with_durable_descriptor(&descriptor)
        .expect("explicit public canonical URL should restore");
    assert_eq!(
        recovered.raw_canonical_url().expose_secret(),
        "https://example.com/public"
    );
    assert_eq!(recovered_store.active_len(), 1);
}

#[test]
fn replayable_recovery_never_derives_raw_url_from_safe_display() {
    let recovered_store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let missing_explicit_material = WebUrlCapabilityDescriptor {
        session_scope_id: SESSION.to_owned(),
        source_id: SOURCE_1.to_owned(),
        durable_entry_id: "message-1".to_owned(),
        safe_display_url: "https://example.com/public".to_owned(),
        restart_policy: ToolRestartPolicy::Replayable,
        replayable_canonical_url: None,
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
    };
    assert_eq!(
        recovered_store.resolve_with_durable_descriptor(&missing_explicit_material),
        Err(UrlCapabilityLookupError::NotFound)
    );
    assert_eq!(recovered_store.active_len(), 0);

    let query_bearing_material = WebUrlCapabilityDescriptor {
        replayable_canonical_url: Some("https://example.com/public?token=secret".to_owned()),
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
        ..missing_explicit_material
    };
    assert_eq!(
        recovered_store.resolve_with_durable_descriptor(&query_bearing_material),
        Err(UrlCapabilityLookupError::NotFound)
    );
    assert_eq!(recovered_store.active_len(), 0);
}

#[test]
fn session_close_removes_staged_live_and_tombstoned_capabilities() {
    let store = WebUrlCapabilityStore::with_limits(SESSION, Duration::from_secs(60), 1)
        .expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");
    stage_and_commit(&store, SOURCE_2, "message-2");
    store
        .stage(registration(
            SOURCE_3,
            "message-3",
            "https://example.com/?token=staged",
            "https://example.com/?[redacted]",
            ToolRestartPolicy::InterruptOnRestart,
        ))
        .expect("registration should stage");
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::Evicted)
    );

    store.close_session();
    assert_eq!(store.active_len(), 0);
    assert_eq!(store.staged_len(), 0);
    assert_eq!(
        store.resolve(SESSION, SOURCE_1),
        Err(UrlCapabilityLookupError::NotFound)
    );
    assert_eq!(
        store.resolve(SESSION, SOURCE_2),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn invalid_or_ambiguous_urls_never_enter_store() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let invalid = [
        "https://user:password@example.com/",
        "ftp://example.com/file",
        "https://example.com:99999/",
        "https://[fe80::1%25eth0]/",
        "https://example.com",
    ];
    for (index, raw_url) in invalid.into_iter().enumerate() {
        assert!(
            store
                .stage(registration(
                    SOURCE_1,
                    &format!("message-{index}"),
                    raw_url,
                    "https://example.com/[redacted]",
                    ToolRestartPolicy::InterruptOnRestart,
                ))
                .is_err(),
            "{raw_url} must be rejected"
        );
    }
    assert_eq!(store.staged_len(), 0);
    assert_eq!(store.active_len(), 0);
}

#[test]
fn safe_display_must_match_the_live_canonical_destination() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let result = store.stage(registration(
        SOURCE_1,
        "message-1",
        "https://actual.example/path?token=secret",
        "https://displayed.example/path?[redacted]",
        ToolRestartPolicy::InterruptOnRestart,
    ));

    assert!(result.is_err());
    assert_eq!(store.staged_len(), 0);
    assert_eq!(store.active_len(), 0);
}

#[test]
fn kernel_projection_stages_commits_and_resolves_sensitive_path_with_shared_policy() {
    let store = Arc::new(WebUrlCapabilityStore::new(SESSION).expect("store should build"));
    let registrar: Arc<dyn UserUrlCapabilityRegistrar> = store.clone();
    let projection = project_user_message_for_persistence_with_nonce(
        "message-projected",
        "fetch https://example.com/files/known-secret-token",
        Some("live-only-nonce"),
        Some(&registrar),
    )
    .expect("kernel projection should stage capability");
    let registration = projection
        .capability_registrations
        .first()
        .expect("projection should register URL");
    assert_eq!(
        registration.safe_display_url,
        "https://example.com/[redacted]"
    );
    assert_eq!(
        registration.restart_policy,
        ToolRestartPolicy::InterruptOnRestart
    );
    store
        .commit_message("message-projected")
        .expect("message should commit");
    let capability = store
        .resolve(SESSION, &registration.source_id)
        .expect("live capability should resolve");
    assert_eq!(
        capability.raw_canonical_url().expose_secret(),
        "https://example.com/files/known-secret-token"
    );
}

#[test]
fn durable_descriptor_must_match_active_capability_binding() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    stage_and_commit(&store, SOURCE_1, "message-1");
    let descriptor = WebUrlCapabilityDescriptor {
        session_scope_id: SESSION.to_owned(),
        source_id: SOURCE_1.to_owned(),
        durable_entry_id: "message-tampered".to_owned(),
        safe_display_url: "https://example.com/message-1?[redacted]".to_owned(),
        restart_policy: ToolRestartPolicy::InterruptOnRestart,
        replayable_canonical_url: None,
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
    };
    assert_eq!(
        store.resolve_with_durable_descriptor(&descriptor),
        Err(UrlCapabilityLookupError::NotFound)
    );
}

#[test]
fn closed_store_cannot_restore_replayable_descriptor() {
    let store = WebUrlCapabilityStore::new(SESSION).expect("store should build");
    let descriptor = WebUrlCapabilityDescriptor {
        session_scope_id: SESSION.to_owned(),
        source_id: SOURCE_1.to_owned(),
        durable_entry_id: "message-1".to_owned(),
        safe_display_url: "https://example.com/public".to_owned(),
        restart_policy: ToolRestartPolicy::Replayable,
        replayable_canonical_url: Some("https://example.com/public".to_owned()),
        originating_call_id: None,
        provenance: WebUrlProvenanceKind::UserMessage,
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
    };
    store.close_session();
    assert_eq!(
        store.resolve_with_durable_descriptor(&descriptor),
        Err(UrlCapabilityLookupError::NotFound)
    );
}
