use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, CompactionConfig, EventHandler, FinalizedHostedTurn,
    HostedCitationCandidate, HostedEvidence, HostedEvidenceProcessor, HostedFinalizationContext,
    HostedSourceCandidate, HostedToolKind, HostedToolLimits, HostedToolRequest, HostedTurnBuffer,
    HostedTurnBufferLimits, HostedTurnError, InteractionMode, MemoryConfig, ModelRequestTimeouts,
    PermissionConfig, PermissionEvaluationContext, ProviderChunk, RunEvent, SecretString, Session,
    ToolRegistry,
};
use sigil_provider_gemini::{GeminiProvider, GeminiProviderConfig};

use super::HostedEvidenceFinalizer;
use super::hosted_terminal_status;

fn context() -> HostedFinalizationContext {
    HostedFinalizationContext {
        session_scope_id: "session-hosted-test".to_owned(),
        provider_name: "provider".to_owned(),
        model_name: "model".to_owned(),
    }
}

fn hosted_start() -> ProviderChunk {
    ProviderChunk::HostedToolStarted {
        authorization_id: "authorization-1".to_owned(),
        invocation_id: "invocation-1".to_owned(),
        kind: HostedToolKind::WebSearch,
    }
}

fn hosted_evidence(evidence: HostedEvidence) -> ProviderChunk {
    ProviderChunk::HostedEvidence {
        authorization_id: "authorization-1".to_owned(),
        invocation_id: "invocation-1".to_owned(),
        kind: HostedToolKind::WebSearch,
        evidence,
    }
}

#[tokio::test]
async fn hosted_finalizer_rewrites_source_id_and_maps_safe_citation_offsets() {
    let mut buffer = HostedTurnBuffer::new(HostedTurnBufferLimits::default());
    buffer
        .push(hosted_start())
        .expect("hosted start should buffer");
    buffer
        .push(ProviderChunk::TextDelta("Rust is fast.".to_owned()))
        .expect("text should buffer");
    buffer
        .push(hosted_evidence(HostedEvidence::Source(
            HostedSourceCandidate::new(
                "provider-source-7",
                "https://example.com/docs?token=secret",
                Some("Docs\u{1b}]8;;https://evil.example\u{7}".to_owned()),
            ),
        )))
        .expect("source should buffer");
    buffer
        .push(hosted_evidence(HostedEvidence::Citation(
            HostedCitationCandidate::new("provider-source-7", 0, 4),
        )))
        .expect("citation should buffer");
    buffer
        .push(hosted_evidence(HostedEvidence::QueryObserved(
            SecretString::new("private query"),
        )))
        .expect("query observation should buffer");

    let finalized = HostedEvidenceFinalizer::new("2026-07-11T00:00:00Z")
        .finalize(context(), &buffer)
        .await
        .expect("valid hosted evidence should finalize");
    assert_eq!(finalized.assistant_text, "Rust is fast.");
    assert!(finalized.hosted_used);
    assert_eq!(
        hosted_terminal_status(&finalized),
        sigil_kernel::HostedToolTerminalStatus::Observed
    );
    assert!(finalized.query_observed);
    assert_eq!(finalized.sources.len(), 1);
    assert!(finalized.sources[0].source_id.starts_with("src_"));
    assert_ne!(finalized.sources[0].source_id, "provider-source-7");
    assert_eq!(
        finalized.sources[0].safe_display_url,
        "https://example.com/docs?[redacted]"
    );
    assert_eq!(finalized.citations.len(), 1);
    assert_eq!(
        (
            finalized.citations[0].start_byte,
            finalized.citations[0].end_byte
        ),
        (0, 4)
    );

    let debug = format!("{buffer:?} {finalized:?}");
    assert!(!debug.contains("private query"));
    assert!(!debug.contains("token=secret"));
    assert!(!debug.contains("provider-source-7"));
}

#[tokio::test]
async fn hosted_finalizer_maps_success_without_search_to_not_used() {
    let buffer = HostedTurnBuffer::new(HostedTurnBufferLimits::default());
    let finalized = HostedEvidenceFinalizer::new("2026-07-11T00:00:00Z")
        .finalize(context(), &buffer)
        .await
        .expect("empty successful provider turn should finalize");
    assert!(!finalized.hosted_used);
    assert_eq!(
        hosted_terminal_status(&finalized),
        sigil_kernel::HostedToolTerminalStatus::NotUsed
    );
}

#[tokio::test]
async fn hosted_finalizer_drops_citation_when_safe_projection_changes_the_span() {
    let mut buffer = HostedTurnBuffer::new(HostedTurnBufferLimits::default());
    buffer
        .push(hosted_start())
        .expect("hosted start should buffer");
    let raw = "token=very-secret-value";
    buffer
        .push(ProviderChunk::TextDelta(raw.to_owned()))
        .expect("text should buffer");
    buffer
        .push(hosted_evidence(HostedEvidence::Source(
            HostedSourceCandidate::new("source", "https://example.com", None),
        )))
        .expect("source should buffer");
    buffer
        .push(hosted_evidence(HostedEvidence::Citation(
            HostedCitationCandidate::new("source", 6, raw.len()),
        )))
        .expect("citation should buffer");

    let finalized = HostedEvidenceFinalizer::new("2026-07-11T00:00:00Z")
        .finalize(context(), &buffer)
        .await
        .expect("safe projection should succeed");
    assert!(!finalized.assistant_text.contains("very-secret-value"));
    assert!(finalized.citations.is_empty());
}

#[tokio::test]
async fn gemini_hosted_provider_evidence_finalizes_to_safe_source_and_unicode_citation()
-> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut bytes = [0u8; 1024];
        loop {
            let Ok(read) = tokio::io::AsyncReadExt::read(&mut socket, &mut bytes).await else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&bytes[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
            data: {\"candidates\":[{\"index\":0,\"content\":{\"parts\":[{\"text\":\"猫🙂 grounded\"}]},\"groundingMetadata\":{\"webSearchQueries\":[\"raw query\"],\"groundingChunks\":[{\"web\":{\"uri\":\"https://example.com/path?token=raw\",\"title\":\"Example\"}}],\"groundingSupports\":[{\"segment\":{\"partIndex\":0,\"startIndex\":0,\"endIndex\":7,\"text\":\"猫🙂\"},\"groundingChunkIndices\":[0]}]}}]}\n\n\
            data: [DONE]\n\n";
        let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
    });
    let provider = GeminiProvider::new(
        GeminiProviderConfig {
            base_url: format!("http://{address}"),
            model: "gemini-2.5-flash".to_owned(),
            api_key: Some("test-key".to_owned()),
        },
        ModelRequestTimeouts::default(),
    )?;
    let hosted = HostedToolRequest::new(
        "auth-runtime-gemini",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let processor = Arc::new(GeminiSinkCheckingFinalizer {
        inner: HostedEvidenceFinalizer::new("2026-07-11T00:00:00Z"),
        events: Arc::clone(&events),
    });
    let input = AgentRunInput::user("search").with_hosted_tools(vec![hosted], processor);
    let workspace = tempfile::tempdir()?;
    let mut session = Session::new("gemini", "gemini-2.5-flash");
    let capability_store = crate::attach_session_url_capability_store(&mut session)?;
    let mut handler = GeminiRecordingHandler {
        events: Arc::clone(&events),
    };
    let output = Agent::new(provider, ToolRegistry::new())
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
                permission_context: PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "猫🙂 grounded");
    assert_eq!(
        events.lock().expect("event lock").as_slice(),
        ["猫🙂 grounded"]
    );
    let provenance = session.external_provenance_entries();
    assert_eq!(provenance.len(), 1);
    assert_eq!(provenance[0].sources.len(), 1);
    assert_eq!(
        provenance[0].sources[0].safe_display_url,
        "https://example.com/path?[redacted]"
    );
    assert_eq!(provenance[0].citations.len(), 1);
    assert_eq!(provenance[0].citations[0].start_byte, 0);
    assert_eq!(provenance[0].citations[0].end_byte, 7);
    let capability = capability_store.resolve(
        session.session_scope_id(),
        &provenance[0].sources[0].source_id,
    )?;
    assert_eq!(
        capability.raw_canonical_url().expose_secret(),
        "https://example.com/path?token=raw"
    );
    let durable = serde_json::to_string(session.entries())?;
    assert!(!durable.contains("raw query"));
    assert!(!durable.contains("token=raw"));
    let debug = format!("{provenance:?}");
    assert!(!debug.contains("raw query"));
    assert!(!debug.contains("token=raw"));
    Ok(())
}

struct GeminiSinkCheckingFinalizer {
    inner: HostedEvidenceFinalizer,
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl HostedEvidenceProcessor for GeminiSinkCheckingFinalizer {
    async fn finalize(
        &self,
        context: HostedFinalizationContext,
        buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError> {
        assert!(
            self.events.lock().expect("event lock").is_empty(),
            "hosted text must not reach a sink before finalization"
        );
        self.inner.finalize(context, buffer).await
    }
}

struct GeminiRecordingHandler {
    events: Arc<Mutex<Vec<String>>>,
}

impl EventHandler for GeminiRecordingHandler {
    fn handle(&mut self, event: RunEvent) -> anyhow::Result<()> {
        match event {
            RunEvent::TextDelta(text) | RunEvent::ReasoningDelta(text) => {
                self.events.lock().expect("event lock").push(text);
            }
            _ => {}
        }
        Ok(())
    }
}
