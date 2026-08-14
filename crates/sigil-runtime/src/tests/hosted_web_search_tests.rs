use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{pin::Pin, sync::Mutex};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;
use sigil_kernel::{
    Agent, AgentHostedTurnPreparer, AgentRunInput, CompletionRequest, DurableEventType,
    HostedCustomToolCompatibility, HostedToolOutcome, HostedToolSupport, HostedToolTerminalStatus,
    HostedWebSearchCapability, InteractionMode, JsonlSessionStore, NoopEventHandler, Provider,
    ProviderCapabilities, ProviderChunk, ProviderRequestRejection, RootConfig, Session,
    SessionStreamRecord, ToolRegistry, ToolRegistryScope, WebSearchRoute,
};

use super::{
    provider_hosted_route_enabled, provider_hosted_safe_destination, safe_provider_origin,
};

#[test]
fn safe_provider_origin_strips_request_paths_and_default_ports() -> Result<()> {
    assert_eq!(
        safe_provider_origin("https://Gemini.Example:443/v1beta")?,
        "https://gemini.example/"
    );
    assert_eq!(
        safe_provider_origin("http://[::1]:4317/provider/v1")?,
        "http://[::1]:4317/"
    );
    Ok(())
}

#[test]
fn safe_provider_origin_rejects_credentials_and_non_http_schemes() {
    assert!(safe_provider_origin("https://token@example.com/v1").is_err());
    assert!(safe_provider_origin("file:///tmp/provider").is_err());
}

#[test]
fn hosted_destination_uses_the_resolved_provider_base_url() -> Result<()> {
    let config: RootConfig = serde_json::from_value(json!({
        "config_version": 2,
        "agent": {
            "connection": "gemini-default",
            "model": "gemini-2.5-pro",
            "tool_timeout_secs": 30
        },
        "connections": {
            "gemini-default": {
                "label": "Gemini",
                "provider": "gemini",
                "protocol": "generate_content",
                "base_url": "https://gemini.example.test/gemini/v1",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_GEMINI_API_KEY"
                }
            }
        }
    }))?;
    assert_eq!(
        provider_hosted_safe_destination(&config, "gemini")?,
        "https://gemini.example.test/"
    );
    Ok(())
}

#[test]
fn hosted_route_falls_back_in_auto_and_rejects_forced_incompatible_composition() -> Result<()> {
    let incompatible = HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        ..HostedWebSearchCapability::default()
    };
    assert!(!provider_hosted_route_enabled(
        WebSearchRoute::Auto,
        incompatible,
        "gemini"
    )?);
    assert!(
        provider_hosted_route_enabled(WebSearchRoute::ProviderHosted, incompatible, "gemini")
            .is_err()
    );

    let compatible = HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
        ..HostedWebSearchCapability::default()
    };
    assert!(provider_hosted_route_enabled(
        WebSearchRoute::Auto,
        compatible,
        "anthropic"
    )?);
    Ok(())
}

struct AcceptingDisclosurePresenter {
    presentations: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct CapturingHostedProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for CapturingHostedProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        crate::provider_capabilities_for_name("deepseek").expect("known provider")
    }

    fn hosted_web_search_capability(&self, _model_name: &str) -> HostedWebSearchCapability {
        HostedWebSearchCapability {
            support: HostedToolSupport::ServerManaged,
            custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
            ..HostedWebSearchCapability::default()
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests.lock().expect("request lock").push(request);
        Ok(Box::pin(stream::iter([
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl sigil_kernel::EgressDisclosurePresenter for AcceptingDisclosurePresenter {
    async fn present(
        &self,
        disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        self.presentations.fetch_add(1, Ordering::SeqCst);
        disclosure.presentation_receipt("deterministic-fake-sink-v1")
    }
}

fn hosted_runtime_root(max_hosted_requests: u64) -> Result<RootConfig> {
    Ok(serde_json::from_value(json!({
        "config_version": 2,
        "agent": {
            "connection": "deepseek-default",
            "model": "deepseek-v4-flash",
            "tool_timeout_secs": 30
        },
        "connections": {
            "deepseek-default": {
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "openai_compat",
                "base_url": "https://api.deepseek.test",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }
        },
        "web": {
            "enabled": true,
            "network_mode": "allow",
            "search_route": "provider_hosted",
            "max_hosted_enabled_provider_requests_per_run": max_hosted_requests
        }
    }))?)
}

fn runtime_hosted_preparer(
    root: &RootConfig,
    session: &Session,
    budget: Arc<sigil_kernel::WebTaskTreeBudget>,
    presentations: Arc<AtomicUsize>,
) -> Result<Arc<dyn AgentHostedTurnPreparer>> {
    Ok(Arc::new(super::RuntimeHostedTurnPreparer {
        root_config: root.clone(),
        presenter: Arc::new(AcceptingDisclosurePresenter { presentations }),
        recorder: session.egress_audit_recorder()?,
        provider_name: "deepseek".to_owned(),
        safe_provider_destination: "https://api.deepseek.test/".to_owned(),
        capability: HostedWebSearchCapability {
            support: HostedToolSupport::ServerManaged,
            custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
            ..HostedWebSearchCapability::default()
        },
        budget,
    }))
}

fn hosted_outcomes(store: &JsonlSessionStore) -> Result<Vec<HostedToolOutcome>> {
    store
        .read_event_records_writer()?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(DurableEventType::HostedToolOutcome) =>
            {
                Some(serde_json::from_value(event.payload.clone()))
            }
            SessionStreamRecord::Stored(_) => None,
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn hosted_authorization_count(store: &JsonlSessionStore) -> Result<usize> {
    Ok(store
        .read_event_records_writer()?
        .iter()
        .filter(|record| {
            record.stored_event().event_kind() == Some(DurableEventType::HostedToolAuthorization)
        })
        .count())
}

#[derive(Debug, thiserror::Error)]
#[error("connect failed before dispatch")]
struct ConnectFailedBeforeDispatch;

#[derive(Clone)]
struct RetryingHostedProvider {
    attempts: Arc<AtomicUsize>,
    failures_before_success: usize,
}

#[async_trait]
impl Provider for RetryingHostedProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        crate::provider_capabilities_for_name("deepseek").expect("known provider")
    }

    fn hosted_web_search_capability(&self, _model_name: &str) -> HostedWebSearchCapability {
        HostedWebSearchCapability {
            support: HostedToolSupport::ServerManaged,
            custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
            ..HostedWebSearchCapability::default()
        }
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        error
            .downcast_ref::<ConnectFailedBeforeDispatch>()
            .map(|_| ProviderRequestRejection::ConnectFailedBeforeDispatch)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < self.failures_before_success {
            return Err(ConnectFailedBeforeDispatch.into());
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct UncertainHostedProvider;

#[async_trait]
impl Provider for UncertainHostedProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        crate::provider_capabilities_for_name("deepseek").expect("known provider")
    }

    fn hosted_web_search_capability(&self, _model_name: &str) -> HostedWebSearchCapability {
        HostedWebSearchCapability {
            support: HostedToolSupport::ServerManaged,
            custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
            ..HostedWebSearchCapability::default()
        }
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        anyhow::bail!("transport outcome is uncertain")
    }
}

#[tokio::test]
async fn hosted_stream_establishment_commits_budget_even_when_search_is_not_used() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let root = hosted_runtime_root(4)?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "hosted-stream-ok",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let preparer = runtime_hosted_preparer(
        &root,
        &session,
        Arc::clone(&budget),
        Arc::new(AtomicUsize::new(0)),
    )?;
    let mut options = crate::build_run_options(
        &root,
        temp.path().to_path_buf(),
        InteractionMode::Interactive,
        None,
    );
    options.max_turns = Some(1);

    Agent::new(
        CapturingHostedProvider {
            requests: Arc::clone(&requests),
        },
        ToolRegistry::new(),
    )
    .run_with_input(
        &mut session,
        AgentRunInput::user("answer without searching").with_hosted_turn_preparer(preparer),
        options,
        &mut NoopEventHandler,
    )
    .await?;

    assert_eq!(budget.snapshot()?.hosted_requests, 1);
    assert_eq!(hosted_authorization_count(&store)?, 1);
    let outcomes = hosted_outcomes(&store)?;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, HostedToolTerminalStatus::NotUsed);
    Ok(())
}

#[tokio::test]
async fn hosted_uncertain_transport_outcome_commits_budget_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let root = hosted_runtime_root(4)?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "hosted-uncertain",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let preparer = runtime_hosted_preparer(
        &root,
        &session,
        Arc::clone(&budget),
        Arc::new(AtomicUsize::new(0)),
    )?;
    let mut options = crate::build_run_options(
        &root,
        temp.path().to_path_buf(),
        InteractionMode::Interactive,
        None,
    );
    options.max_turns = Some(1);

    Agent::new(UncertainHostedProvider, ToolRegistry::new())
        .run_with_input(
            &mut session,
            AgentRunInput::user("search").with_hosted_turn_preparer(preparer),
            options,
            &mut NoopEventHandler,
        )
        .await
        .expect_err("uncertain transport must fail the provider turn");

    assert_eq!(budget.snapshot()?.hosted_requests, 1);
    let outcomes = hosted_outcomes(&store)?;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, HostedToolTerminalStatus::RequestFailed);
    Ok(())
}

#[tokio::test]
async fn hosted_connect_retry_reuses_one_provisional_reservation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let root = hosted_runtime_root(4)?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "hosted-connect-retry",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let attempts = Arc::new(AtomicUsize::new(0));
    let preparer = runtime_hosted_preparer(
        &root,
        &session,
        Arc::clone(&budget),
        Arc::new(AtomicUsize::new(0)),
    )?;
    let mut options = crate::build_run_options(
        &root,
        temp.path().to_path_buf(),
        InteractionMode::Interactive,
        None,
    );
    options.max_turns = Some(1);

    Agent::new(
        RetryingHostedProvider {
            attempts: Arc::clone(&attempts),
            failures_before_success: 1,
        },
        ToolRegistry::new(),
    )
    .run_with_input(
        &mut session,
        AgentRunInput::user("search").with_hosted_turn_preparer(preparer),
        options,
        &mut NoopEventHandler,
    )
    .await?;

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(budget.snapshot()?.hosted_requests, 1);
    assert_eq!(hosted_authorization_count(&store)?, 1);
    assert_eq!(hosted_outcomes(&store)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn hosted_connect_failure_before_dispatch_refunds_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let root = hosted_runtime_root(4)?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "hosted-connect-failed",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let attempts = Arc::new(AtomicUsize::new(0));
    let preparer = runtime_hosted_preparer(
        &root,
        &session,
        Arc::clone(&budget),
        Arc::new(AtomicUsize::new(0)),
    )?;
    let mut options = crate::build_run_options(
        &root,
        temp.path().to_path_buf(),
        InteractionMode::Interactive,
        None,
    );
    options.max_turns = Some(1);

    Agent::new(
        RetryingHostedProvider {
            attempts: Arc::clone(&attempts),
            failures_before_success: usize::MAX,
        },
        ToolRegistry::new(),
    )
    .run_with_input(
        &mut session,
        AgentRunInput::user("search").with_hosted_turn_preparer(preparer),
        options,
        &mut NoopEventHandler,
    )
    .await
    .expect_err("all pre-dispatch connect attempts must fail");

    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(budget.snapshot()?.hosted_requests, 0);
    assert_eq!(hosted_authorization_count(&store)?, 1);
    let outcomes = hosted_outcomes(&store)?;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, HostedToolTerminalStatus::RequestFailed);
    Ok(())
}

#[tokio::test]
async fn hosted_preparer_skips_turn_when_hosted_budget_is_exhausted() -> Result<()> {
    // A run whose hosted-request budget is already spent must skip the hosted turn softly
    // (return None) so the run continues without web search instead of failing.
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let recorder = session.egress_audit_recorder()?;
    let root: RootConfig = serde_json::from_value(json!({
        "config_version": 2,
        "agent": {
            "connection": "deepseek-default",
            "model": "deepseek-v4-flash",
            "tool_timeout_secs": 30
        },
        "connections": {
            "deepseek-default": {
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "openai_compat",
                "base_url": "https://api.deepseek.test",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }
        },
        "web": {
            "enabled": true,
            "search_route": "provider_hosted",
            "max_hosted_enabled_provider_requests_per_run": 1
        }
    }))?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "web-run-exhausted",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let mut reservation = budget.reserve(sigil_kernel::WebBudgetReservationRequest {
        correlation_id: "hosted-correlation-1".to_owned(),
        attempt_id: "hosted-attempt-1".to_owned(),
        route_lease_id: "hosted-lease-1".to_owned(),
        route_fingerprint: "hosted-fp-1".to_owned(),
        kind: sigil_kernel::WebBudgetReservationKind::HostedProviderRequest,
    })?;
    reservation.commit_call()?;
    drop(reservation);

    let presentations = Arc::new(AtomicUsize::new(0));
    let preparer = super::RuntimeHostedTurnPreparer {
        root_config: root,
        presenter: Arc::new(AcceptingDisclosurePresenter {
            presentations: Arc::clone(&presentations),
        }),
        recorder,
        provider_name: "deepseek".to_owned(),
        safe_provider_destination: "https://api.deepseek.test/".to_owned(),
        capability: HostedWebSearchCapability::default(),
        budget,
    };
    let turn = preparer.prepare_turn().await?;
    assert!(
        turn.is_none(),
        "an exhausted hosted-request budget must skip the hosted turn"
    );
    assert_eq!(
        presentations.load(Ordering::SeqCst),
        0,
        "an unavailable request must not disclose egress that cannot occur"
    );
    assert!(
        store
            .read_event_records_writer()?
            .iter()
            .all(|record| record.stored_event().event_kind()
                != Some(DurableEventType::EgressDisclosurePresented))
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_scope_neither_injects_nor_charges_hidden_hosted_search() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let root: RootConfig = serde_json::from_value(json!({
        "config_version": 2,
        "agent": {
            "connection": "deepseek-default",
            "model": "deepseek-v4-flash",
            "tool_timeout_secs": 30
        },
        "connections": {
            "deepseek-default": {
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "openai_compat",
                "base_url": "https://api.deepseek.test",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }
        },
        "web": {
            "enabled": true,
            "network_mode": "allow",
            "search_route": "provider_hosted",
            "max_hosted_enabled_provider_requests_per_run": 1
        }
    }))?;
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        "plan-review-hosted-budget",
        super::super::remote_mcp::web_budget_limits(&root),
        None,
    )?;
    let presentations = Arc::new(AtomicUsize::new(0));
    let presenter: Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
        Arc::new(AcceptingDisclosurePresenter {
            presentations: Arc::clone(&presentations),
        });
    let mut registry = ToolRegistry::new();
    crate::web_search_tool::register_web_search_tool(
        &mut registry,
        &root,
        64,
        Arc::clone(&presenter),
    );
    registry.set_run_input_preparer_for_tools(
        Arc::new(super::HostedWebSearchInputPreparer::new(
            root.clone(),
            presenter,
        )),
        ToolRegistryScope::from_names_and_prefixes(["websearch"], std::iter::empty::<&str>()),
    );
    let scoped = crate::build_plan_review_tool_registry(&registry, &root).into_registry();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingHostedProvider {
            requests: Arc::clone(&requests),
        },
        scoped,
    );
    let mut options = crate::build_run_options(
        &root,
        temp.path().to_path_buf(),
        InteractionMode::Interactive,
        None,
    );
    options.max_turns = Some(1);

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect the workspace")
                .with_web_task_tree_budget(Arc::clone(&budget)),
            options,
            &mut NoopEventHandler,
        )
        .await?;

    let requests = requests.lock().expect("request lock");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].hosted_tools.is_empty());
    assert_eq!(presentations.load(Ordering::SeqCst), 0);
    assert_eq!(budget.snapshot()?.hosted_requests, 0);
    assert!(
        store
            .read_event_records_writer()?
            .iter()
            .all(|record| record.stored_event().event_kind()
                != Some(DurableEventType::EgressDisclosurePresented))
    );
    Ok(())
}
