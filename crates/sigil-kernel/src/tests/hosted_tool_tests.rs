use super::*;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, CompactionConfig, CompletionRequest, EventHandler,
    FinalizedHostedTurn, HostedCitationCandidate, HostedFinalizationContext, HostedSourceCandidate,
    HostedToolLimits, HostedToolRequest, HostedToolSupport, InteractionMode, MemoryConfig,
    PermissionConfig, Provider, ProviderCapabilities, ReasoningStreamSupport, RunCancellationOwner,
    RunEvent, SecretString, Session, ToolRegistry,
};

fn hosted_request() -> HostedToolRequest {
    HostedToolRequest::new(
        "authorization-1",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("fixture request is valid")
}

#[test]
fn hosted_tool_turn_buffer_rejects_limit_plus_one_without_retaining_raw_text() {
    let mut buffer = HostedTurnBuffer::new(HostedTurnBufferLimits {
        total_bytes: 4,
        text_bytes: 4,
        reasoning_bytes: 4,
        evidence_bytes: 4,
        evidence_items: 1,
    });
    buffer
        .push(ProviderChunk::TextDelta("safe".to_owned()))
        .expect("limit-sized delta should fit");
    let error = buffer
        .push(ProviderChunk::TextDelta("x".to_owned()))
        .expect_err("limit plus one must fail closed");
    assert_eq!(error, HostedTurnError::BufferLimitExceeded);
    assert_eq!(buffer.text(), "safe");
}

#[test]
fn hosted_tool_evidence_debug_redacts_url_title_query_and_source_id() {
    let evidence = vec![
        HostedEvidence::Source(HostedSourceCandidate::new(
            "remote-secret-id",
            "https://example.com/?token=raw-secret",
            Some("raw secret title".to_owned()),
        )),
        HostedEvidence::Citation(HostedCitationCandidate::new("remote-secret-id", 0, 4)),
        HostedEvidence::QueryObserved(SecretString::new("private query")),
    ];
    let debug = format!("{evidence:?}");
    for secret in [
        "remote-secret-id",
        "raw-secret",
        "raw secret title",
        "private query",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn hosted_tool_request_wire_state_allows_retry_only_before_request_bytes() {
    let mut state = HostedRequestWireState::Prepared;
    assert!(state.retry_allowed());
    state
        .mark_request_bytes_started()
        .expect("first request byte transition should succeed");
    assert!(!state.retry_allowed());
    assert_eq!(
        state.mark_request_bytes_started(),
        Err(HostedWireStateError::InvalidTransition)
    );
    state.finish().expect("active request may finish");
    assert!(!state.retry_allowed());
}

#[test]
fn hosted_tool_request_fingerprint_binds_authorization_kind_and_limits() {
    let limits = HostedToolLimits {
        max_uses: Some(3),
        allowed_domains: vec!["docs.example.com/reference".to_owned()],
        blocked_domains: Vec::new(),
    };
    let request =
        HostedToolRequest::new("authorization-1", HostedToolKind::WebSearch, limits.clone())
            .expect("request should validate");
    request
        .validate()
        .expect("bound fingerprint should validate");

    let mut drifted = request.clone();
    drifted.limits.max_uses = Some(4);
    assert_eq!(
        drifted.validate(),
        Err(HostedToolRequestError::RequestFingerprintMismatch)
    );

    let reordered = HostedToolRequest::new(
        "authorization-1",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            allowed_domains: vec!["b.example.com".to_owned(), "a.example.com".to_owned()],
            ..HostedToolLimits::default()
        },
    )
    .expect("request should validate");
    let canonical_order = HostedToolRequest::new(
        "authorization-1",
        HostedToolKind::WebSearch,
        HostedToolLimits {
            allowed_domains: vec!["a.example.com".to_owned(), "b.example.com".to_owned()],
            ..HostedToolLimits::default()
        },
    )
    .expect("request should validate");
    assert_eq!(
        reordered.request_fingerprint,
        canonical_order.request_fingerprint
    );

    let serialized = serde_json::to_string(&request).expect("request should serialize");
    assert!(serialized.contains(&request.request_fingerprint));
    assert!(serialized.contains("docs.example.com/reference"));
}

#[test]
fn hosted_tool_limits_reject_hostile_ambiguous_and_oversize_domains() {
    for domain in [
        "https://example.com",
        "user@example.com",
        "*.example.com",
        "EXAMPLE.com",
        "example.com?secret=value",
        "localhost",
        "examp\nle.com",
        "аmazon.com",
    ] {
        let limits = HostedToolLimits {
            allowed_domains: vec![domain.to_owned()],
            ..HostedToolLimits::default()
        };
        assert_eq!(
            limits.validate(),
            Err(HostedToolRequestError::InvalidDomainFilter),
            "domain {domain:?} should fail closed"
        );
    }
    assert_eq!(
        HostedToolLimits {
            allowed_domains: vec!["example.com".to_owned()],
            blocked_domains: vec!["blocked.example.com".to_owned()],
            ..HostedToolLimits::default()
        }
        .validate(),
        Err(HostedToolRequestError::ConflictingDomainFilters)
    );
    assert_eq!(
        HostedToolLimits {
            max_uses: Some(0),
            ..HostedToolLimits::default()
        }
        .validate(),
        Err(HostedToolRequestError::ZeroMaxUses)
    );
    assert_eq!(
        HostedToolLimits {
            allowed_domains: vec!["example.com".to_owned(); 101],
            ..HostedToolLimits::default()
        }
        .validate(),
        Err(HostedToolRequestError::DomainFilterLimitExceeded)
    );
}

#[test]
fn hosted_turn_buffer_requires_bounded_started_invocation_correlation() {
    let mut buffer = HostedTurnBuffer::new(HostedTurnBufferLimits::default());
    assert_eq!(
        buffer.push(ProviderChunk::HostedEvidence {
            authorization_id: "authorization-1".to_owned(),
            invocation_id: "invocation-1".to_owned(),
            kind: HostedToolKind::WebSearch,
            evidence: HostedEvidence::QueryObserved(SecretString::new("query")),
        }),
        Err(HostedTurnError::InvalidInvocationCorrelation)
    );
    assert_eq!(
        buffer.push(ProviderChunk::HostedToolStarted {
            authorization_id: "authorization-1".to_owned(),
            invocation_id: "bad\nlog".to_owned(),
            kind: HostedToolKind::WebSearch,
        }),
        Err(HostedTurnError::InvalidInvocationCorrelation)
    );
    assert_eq!(
        buffer.push(ProviderChunk::HostedToolStarted {
            authorization_id: "a".repeat(513),
            invocation_id: "invocation-1".to_owned(),
            kind: HostedToolKind::WebSearch,
        }),
        Err(HostedTurnError::InvalidInvocationCorrelation)
    );
}

struct RawHostedProvider;

#[async_trait]
impl Provider for RawHostedProvider {
    fn name(&self) -> &str {
        "raw-hosted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: false,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    fn hosted_web_search_capability(&self, _model_name: &str) -> HostedWebSearchCapability {
        HostedWebSearchCapability {
            support: HostedToolSupport::ServerManaged,
            ..HostedWebSearchCapability::default()
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        assert_eq!(request.hosted_tools, vec![hosted_request()]);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::HostedToolStarted {
                authorization_id: "authorization-1".to_owned(),
                invocation_id: "invocation-1".to_owned(),
                kind: HostedToolKind::WebSearch,
            }),
            Ok(ProviderChunk::TextDelta("raw token=private".to_owned())),
            Ok(ProviderChunk::HostedEvidence {
                authorization_id: "authorization-1".to_owned(),
                invocation_id: "invocation-1".to_owned(),
                kind: HostedToolKind::WebSearch,
                evidence: HostedEvidence::Source(HostedSourceCandidate::new(
                    "raw-source",
                    "https://example.com/?token=private",
                    Some("raw title".to_owned()),
                )),
            }),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct VisibilityCheckingProcessor {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl HostedEvidenceProcessor for VisibilityCheckingProcessor {
    async fn finalize(
        &self,
        _context: HostedFinalizationContext,
        buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError> {
        assert!(self.events.lock().expect("event lock").is_empty());
        assert!(buffer.text().contains("private"));
        Ok(FinalizedHostedTurn {
            assistant_text: "safe answer".to_owned(),
            reasoning_trace: "safe reasoning".to_owned(),
            sources: Vec::new(),
            citations: Vec::new(),
            url_capability_registrations: Vec::new(),
            hosted_used: true,
            query_observed: false,
        })
    }
}

struct RecordingHandler {
    events: Arc<Mutex<Vec<String>>>,
}

impl EventHandler for RecordingHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(text) | RunEvent::ReasoningDelta(text) => {
                self.events.lock().expect("event lock").push(text)
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::test]
async fn hosted_tool_agent_emits_and_persists_only_finalized_text() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let events = Arc::new(Mutex::new(Vec::new()));
    let processor = Arc::new(VisibilityCheckingProcessor {
        events: Arc::clone(&events),
    });
    let mut handler = RecordingHandler {
        events: Arc::clone(&events),
    };
    let mut session = Session::new("raw-hosted", "model");
    let agent = Agent::new(RawHostedProvider, ToolRegistry::new());
    let input = AgentRunInput::user("search").with_hosted_tools(vec![hosted_request()], processor);
    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: workspace.path().to_owned(),
                max_turns: Some(1),
                tool_timeout_secs: 30,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await
        .expect("hosted turn should finalize");
    assert_eq!(output.result.final_text, "safe answer");
    assert_eq!(
        events.lock().expect("event lock").as_slice(),
        ["safe reasoning", "safe answer"]
    );
    let durable = serde_json::to_string(session.entries()).expect("session should serialize");
    assert!(durable.contains("safe answer"));
    assert!(!durable.contains("private"));
    assert!(!durable.contains("raw title"));
    assert!(!durable.contains("raw-source"));
}

fn hosted_options(workspace_root: &std::path::Path) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: workspace_root.to_owned(),
        max_turns: Some(1),
        tool_timeout_secs: 30,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

struct FailingProcessor;

#[async_trait]
impl HostedEvidenceProcessor for FailingProcessor {
    async fn finalize(
        &self,
        _context: HostedFinalizationContext,
        _buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError> {
        Err(HostedTurnError::FinalizationFailed)
    }
}

#[tokio::test]
async fn hosted_tool_missing_processor_fails_before_raw_provider_output() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut handler = RecordingHandler {
        events: Arc::clone(&events),
    };
    let mut session = Session::new("raw-hosted", "model");
    Agent::new(RawHostedProvider, ToolRegistry::new())
        .run_with_input(
            &mut session,
            AgentRunInput::user("search").with_hosted_tool_requests(vec![hosted_request()]),
            hosted_options(workspace.path()),
            &mut handler,
        )
        .await
        .expect_err("missing processor must fail closed");
    assert!(events.lock().expect("event lock").is_empty());
    let durable = serde_json::to_string(session.entries()).expect("session should serialize");
    assert!(!durable.contains("private"));
}

#[tokio::test]
async fn hosted_tool_finalizer_error_emits_no_raw_delta() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut handler = RecordingHandler {
        events: Arc::clone(&events),
    };
    let mut session = Session::new("raw-hosted", "model");
    Agent::new(RawHostedProvider, ToolRegistry::new())
        .run_with_input(
            &mut session,
            AgentRunInput::user("search")
                .with_hosted_tools(vec![hosted_request()], Arc::new(FailingProcessor)),
            hosted_options(workspace.path()),
            &mut handler,
        )
        .await
        .expect_err("finalizer failure must terminate the hosted turn");
    assert!(events.lock().expect("event lock").is_empty());
    let durable = serde_json::to_string(session.entries()).expect("session should serialize");
    assert!(!durable.contains("private"));
    assert!(!durable.contains("raw title"));
}

struct CancellingHostedProvider {
    owner: Arc<RunCancellationOwner>,
}

#[async_trait]
impl Provider for CancellingHostedProvider {
    fn name(&self) -> &str {
        "cancelling-hosted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        RawHostedProvider.capabilities()
    }

    fn hosted_web_search_capability(&self, model_name: &str) -> HostedWebSearchCapability {
        RawHostedProvider.hosted_web_search_capability(model_name)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let owner = Arc::clone(&self.owner);
        Ok(Box::pin(
            stream::iter(vec![
                ProviderChunk::TextDelta("raw private cancellation text".to_owned()),
                ProviderChunk::Done,
            ])
            .map(move |chunk| {
                if matches!(chunk, ProviderChunk::Done) {
                    owner.request_cancel();
                }
                Ok(chunk)
            }),
        ))
    }
}

#[tokio::test]
async fn hosted_tool_cancel_discards_buffer_before_finalizer_visibility() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut handler = RecordingHandler {
        events: Arc::clone(&events),
    };
    let owner = Arc::new(RunCancellationOwner::new());
    let mut session = Session::new("cancelling-hosted", "model");
    Agent::new(
        CancellingHostedProvider {
            owner: Arc::clone(&owner),
        },
        ToolRegistry::new(),
    )
    .run_with_input(
        &mut session,
        AgentRunInput::user("search")
            .with_cancellation(owner.handle())
            .with_hosted_tools(
                vec![hosted_request()],
                Arc::new(VisibilityCheckingProcessor {
                    events: Arc::clone(&events),
                }),
            ),
        hosted_options(workspace.path()),
        &mut handler,
    )
    .await
    .expect_err("cancellation must win before finalization");
    assert!(events.lock().expect("event lock").is_empty());
    let durable = serde_json::to_string(session.entries()).expect("session should serialize");
    assert!(!durable.contains("raw private cancellation text"));
}
