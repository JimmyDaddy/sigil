use tempfile::tempdir;

use super::*;
use crate::session::SessionWriterFault;
use crate::{JsonlSessionStore, Session};

fn durable_session() -> (tempfile::TempDir, JsonlSessionStore, EgressAuditRecorder) {
    let temp = tempdir().expect("temp dir");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl")).expect("store");
    let session = Session::new("provider", "model").with_store(store.clone());
    let recorder = session.egress_audit_recorder().expect("egress recorder");
    (temp, store, recorder)
}

fn hosted_authorization() -> HostedToolAuthorization {
    HostedToolAuthorization {
        record_id: "hosted-authorization-1".to_owned(),
        root_run_id: "root-run-1".to_owned(),
        correlation_id: "hosted-correlation-1".to_owned(),
        authorization_id: "hosted-auth-1".to_owned(),
        route_lease_id: "hosted-route-lease-1".to_owned(),
        hosted_request_fingerprint: "hosted-fingerprint-1".to_owned(),
        provider_name: "gemini".to_owned(),
        model_name: "gemini-test".to_owned(),
        effect: crate::ApprovalMode::Allow,
        scope: HostedAuthorizationScope::ProviderRequest,
    }
}

fn query_started() -> QueryEgressStarted {
    QueryEgressStarted {
        record_id: "query-start-1".to_owned(),
        root_run_id: "root-run-1".to_owned(),
        correlation_id: "query-correlation-1".to_owned(),
        route_lease_id: "route-lease-1".to_owned(),
        route_fingerprint: "route-fingerprint-1".to_owned(),
        query_chars: 6,
        query_bytes: 6,
        egress_class: WebQueryEgressClass::UserProvided,
    }
}

fn query_disclosure(correlation: &str, disclosure_id: &str) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some(correlation.to_owned()),
        disclosure_id,
        "tui",
        "Web search",
        "route-fingerprint-1",
        "profile-fingerprint-1",
        "https://example.com/",
        "https://example.com/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("query disclosure")
}

#[test]
fn egress_audit_appends_strict_receipts_and_unique_query_terminal() {
    let (_temp, store, recorder) = durable_session();
    let start = query_started();
    recorder
        .append_query_started(&start)
        .expect("durable query start receipt");
    let outcome = QueryEgressOutcome {
        record_id: "query-outcome-1".to_owned(),
        root_run_id: start.root_run_id.clone(),
        correlation_id: start.correlation_id.clone(),
        route_fingerprint: start.route_fingerprint.clone(),
        status: QueryEgressTerminalStatus::Completed,
        error_class: None,
    };
    assert!(recorder.append_query_outcome(&outcome).expect("outcome"));
    assert!(!recorder.append_query_outcome(&outcome).expect("idempotent"));

    let conflicting = QueryEgressOutcome {
        record_id: "query-outcome-conflicting".to_owned(),
        status: QueryEgressTerminalStatus::Failed,
        error_class: Some(WebSearchFailureClass::Timeout),
        ..outcome
    };
    assert!(recorder.append_query_outcome(&conflicting).is_err());

    let events = store.read_event_records_writer().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(
                |record| matches!(record, crate::SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(crate::DurableEventType::QueryEgressOutcome))
            )
            .count(),
        1
    );
}

#[test]
fn disclosure_receipt_is_one_shot_and_rejects_old_binding() {
    let first = query_disclosure("query-correlation-1", "disclosure-1");
    let second = query_disclosure("query-correlation-1", "disclosure-2");
    let old_receipt = first.presentation_receipt("tui-frame-v1").expect("receipt");
    assert!(matches!(
        validate_disclosure_receipt(&second, old_receipt),
        Err(EgressAuditError::ReceiptMismatch)
    ));

    let receipt = second
        .presentation_receipt("tui-frame-v1")
        .expect("receipt");
    let presented = validate_disclosure_receipt(&second, receipt).expect("presented");
    assert_eq!(presented.disclosure_id, "disclosure-2");
    assert_eq!(
        presented.correlation_id.as_deref(),
        Some("query-correlation-1")
    );
}

#[test]
fn disclosure_rejects_query_bearing_destination_and_secret_carrier() {
    let error = PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some("query-correlation-1".to_owned()),
        "disclosure-1",
        "tui",
        "Web search token=secret",
        "route-fingerprint-1",
        "profile-fingerprint-1",
        "https://example.com/?token=secret",
        "https://example.com/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect_err("unsafe disclosure must fail");
    assert!(matches!(error, EgressAuditError::InvalidRecord(_)));
}

#[test]
fn recovery_appends_interrupted_once_for_hosted_and_query_without_replay() {
    let (_temp, store, recorder) = durable_session();
    recorder
        .append_hosted_authorization(&hosted_authorization())
        .expect("hosted authorization");
    recorder
        .append_query_started(&query_started())
        .expect("query start");

    let session =
        Session::load_from_store("fallback", "fallback", store.clone()).expect("first recovery");
    assert_eq!(
        session
            .reconcile_egress_lifecycle()
            .expect("first recovery"),
        2,
        "run start closes both open egress lifecycles"
    );
    let session =
        Session::load_from_store("fallback", "fallback", store.clone()).expect("second recovery");
    assert_eq!(
        session
            .reconcile_egress_lifecycle()
            .expect("second recovery"),
        0,
        "repeated recovery must be idempotent"
    );

    let events = store.read_event_records_writer().expect("events");
    let hosted_outcomes = events
        .iter()
        .filter(|record| {
            matches!(record, crate::SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(crate::DurableEventType::HostedToolOutcome))
        })
        .count();
    let query_outcomes = events
        .iter()
        .filter(|record| {
            matches!(record, crate::SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(crate::DurableEventType::QueryEgressOutcome))
        })
        .count();
    assert_eq!(hosted_outcomes, 1);
    assert_eq!(query_outcomes, 1);
    assert_eq!(
        events
            .iter()
            .filter(
                |record| matches!(record, crate::SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(crate::DurableEventType::QueryEgressStarted))
            )
            .count(),
        1,
        "recovery must never replay a query start"
    );
}

#[test]
fn session_load_never_marks_an_in_flight_hosted_authorization_interrupted() {
    // Regression: a concurrent session load (display/control/GC) during an active hosted turn
    // used to reconcile Interrupted for the in-flight authorization, after which the real
    // finalizer terminal was rejected as "already has a different terminal outcome" and the
    // whole run failed. Loads must never append egress terminals; only run start may.
    let (_temp, store, recorder) = durable_session();
    let authorization = hosted_authorization();
    recorder
        .append_hosted_authorization(&authorization)
        .expect("hosted authorization");

    // An unrelated load happens while the hosted request is still in flight; the load itself
    // must not append any terminal outcome.
    let _session =
        Session::load_from_store("fallback", "fallback", store.clone()).expect("concurrent load");
    let events = store.read_event_records_writer().expect("events");
    assert!(
        !events.iter().any(|record| {
            matches!(record, crate::SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(crate::DurableEventType::HostedToolOutcome))
        }),
        "no outcome may exist while the turn is in flight"
    );

    // The finalizer's real terminal is still accepted after the concurrent load.
    recorder
        .append_hosted_outcome(&HostedToolOutcome {
            record_id: "hosted-outcome-observed".to_owned(),
            root_run_id: authorization.root_run_id,
            correlation_id: authorization.correlation_id,
            authorization_id: authorization.authorization_id,
            hosted_request_fingerprint: authorization.hosted_request_fingerprint,
            status: HostedToolTerminalStatus::Observed,
        })
        .expect("real terminal outcome must still be accepted");

    // A later run start sees the authorization terminal and appends nothing new.
    let session =
        Session::load_from_store("fallback", "fallback", store.clone()).expect("next run start");
    assert_eq!(
        session
            .reconcile_egress_lifecycle()
            .expect("next run start"),
        0,
        "terminal lifecycle must not be re-reconciled"
    );
}

#[test]
fn durable_sync_failure_returns_no_permit_and_recovery_closes_visible_start() {
    let (_temp, store, recorder) = durable_session();
    store
        .inject_writer_fault(SessionWriterFault::BeforeSync)
        .expect("inject sync failure");
    assert!(matches!(
        recorder.append_query_started(&query_started()),
        Err(EgressAuditError::Store(_))
            | Err(EgressAuditError::Durable(DurableAuditError::AppendFailed(
                _
            )))
    ));
    let session =
        Session::load_from_store("fallback", "fallback", store.clone()).expect("recovery");
    session
        .reconcile_egress_lifecycle()
        .expect("run-start recovery closes the visible query start");
    let events = store
        .read_event_records_writer()
        .expect("recovered records");
    assert!(events.iter().any(|record| matches!(record,
        crate::SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(crate::DurableEventType::QueryEgressStarted)
    )));
    assert!(events.iter().any(|record| matches!(record,
        crate::SessionStreamRecord::Stored(event)
            if event.event_kind() == Some(crate::DurableEventType::QueryEgressOutcome)
    )));
}

#[test]
fn in_memory_session_cannot_fabricate_an_egress_recorder() {
    let session = Session::new("provider", "model");
    assert!(matches!(
        session.egress_audit_recorder(),
        Err(DurableAuditError::MissingDurableStore)
    ));
}
