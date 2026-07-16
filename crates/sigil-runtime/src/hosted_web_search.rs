use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentHostedTurn, AgentHostedTurnPreparer, AgentRunInput, AgentRunInputPreparer, ApprovalMode,
    EgressAuditRecorder, EgressDataCategory, EgressDisclosureKind, EgressDisclosurePresenter,
    EgressNetworkRoute, FinalizedHostedTurn, HostedConstraintEnforcement,
    HostedCustomToolCompatibility, HostedEvidenceProcessor, HostedFinalizationContext,
    HostedToolAuthorization, HostedToolKind, HostedToolLimits, HostedToolRequest,
    HostedToolTerminalStatus, HostedTurnBuffer, HostedTurnError, HostedWebSearchCapability,
    NetworkPolicy, PreEgressDisclosure, Provider, RootConfig, Session, WebBudgetReservationKind,
    WebBudgetReservationRequest, WebSearchRoute, WebTaskTreeBudget, validate_disclosure_receipt,
};
use url::Url;

use crate::{
    ActiveHostedEgress, EgressOrderingCoordinator, HostedEvidenceFinalizer, hosted_terminal_status,
};

pub(crate) struct HostedWebSearchInputPreparer {
    root_config: RootConfig,
    presenter: Arc<dyn EgressDisclosurePresenter>,
}

impl HostedWebSearchInputPreparer {
    pub(crate) fn new(
        root_config: RootConfig,
        presenter: Arc<dyn EgressDisclosurePresenter>,
    ) -> Self {
        Self {
            root_config,
            presenter,
        }
    }
}

#[async_trait]
impl AgentRunInputPreparer for HostedWebSearchInputPreparer {
    async fn prepare(
        &self,
        provider: &dyn Provider,
        session: &Session,
        input: AgentRunInput,
    ) -> Result<AgentRunInput> {
        if !self.root_config.web.enabled || self.root_config.web.network_mode == NetworkPolicy::Deny
        {
            return Ok(input);
        }
        let budget = match input.web_task_tree_budget() {
            Some(budget) => budget,
            None => WebTaskTreeBudget::new(
                format!("web-run-{}", uuid::Uuid::new_v4()),
                crate::remote_mcp::web_budget_limits(&self.root_config),
                None,
            )?,
        };
        let input = input.with_web_task_tree_budget(Arc::clone(&budget));
        if !matches!(
            self.root_config.web.search_route,
            WebSearchRoute::Auto | WebSearchRoute::ProviderHosted
        ) {
            return Ok(input);
        }
        let capability = provider.hosted_web_search_capability(&self.root_config.agent.model);
        if !capability.is_supported() {
            return Ok(input);
        }
        if !provider_hosted_route_enabled(
            self.root_config.web.search_route,
            capability,
            provider.name(),
        )? {
            return Ok(input);
        }
        // Hosted requests do not pass through an ordinary client-tool approval round trip. Keep
        // `ask` fail-closed until that explicit product interaction exists.
        if self.root_config.web.network_mode == NetworkPolicy::Ask {
            return Ok(input.suppress_tool("websearch"));
        }
        let safe_provider_destination =
            provider_hosted_safe_destination(&self.root_config, provider.name())?;

        let preparer = Arc::new(RuntimeHostedTurnPreparer {
            root_config: self.root_config.clone(),
            presenter: Arc::clone(&self.presenter),
            recorder: session.egress_audit_recorder()?,
            provider_name: provider.name().to_owned(),
            safe_provider_destination,
            capability,
            budget,
        });
        Ok(input
            .suppress_tool("websearch")
            .with_hosted_turn_preparer(preparer))
    }
}

struct RuntimeHostedTurnPreparer {
    root_config: RootConfig,
    presenter: Arc<dyn EgressDisclosurePresenter>,
    recorder: EgressAuditRecorder,
    provider_name: String,
    safe_provider_destination: String,
    capability: HostedWebSearchCapability,
    budget: Arc<WebTaskTreeBudget>,
}

#[async_trait]
impl AgentHostedTurnPreparer for RuntimeHostedTurnPreparer {
    async fn prepare_turn(&self) -> Result<AgentHostedTurn> {
        let unique = uuid::Uuid::new_v4();
        let root_run_id = self.budget.root_run_id().to_owned();
        let correlation_id = format!("hosted-web-correlation-{unique}");
        let authorization_id = format!("hosted-web-authorization-{unique}");
        let route_lease_id = format!("hosted-web-lease-{unique}");
        let request = HostedToolRequest::new(
            authorization_id.clone(),
            HostedToolKind::WebSearch,
            hosted_limits(&self.root_config, self.capability),
        )?;
        let route_fingerprint = sha256(&format!(
            "hosted-web\0{}\0{}\0{}",
            self.provider_name, self.root_config.agent.model, request.request_fingerprint
        ));
        let disclosure = PreEgressDisclosure::new(
            EgressDisclosureKind::Query,
            Some(correlation_id.clone()),
            format!("hosted-web-disclosure-{unique}"),
            "tui_or_cli",
            format!("{} provider-hosted web search", self.provider_name),
            route_fingerprint.clone(),
            sha256(&format!("hosted-web-profile\0{route_fingerprint}")),
            self.safe_provider_destination.clone(),
            self.safe_provider_destination.clone(),
            EgressNetworkRoute::Direct,
            vec![
                EgressDataCategory::SearchQuery,
                EgressDataCategory::ConnectionMetadata,
            ],
        )?;
        let receipt = self.presenter.present(disclosure.clone()).await?;
        self.recorder
            .append_disclosure_presented(&validate_disclosure_receipt(&disclosure, receipt)?)?;

        let reservation = self.budget.reserve(WebBudgetReservationRequest {
            correlation_id: correlation_id.clone(),
            attempt_id: format!("hosted-web-attempt-{unique}"),
            route_lease_id: route_lease_id.clone(),
            route_fingerprint: request.request_fingerprint.clone(),
            kind: WebBudgetReservationKind::HostedProviderRequest,
        })?;
        let authorization = HostedToolAuthorization {
            record_id: format!("hosted-web-authorization-record-{unique}"),
            root_run_id,
            correlation_id,
            authorization_id,
            route_lease_id,
            hosted_request_fingerprint: request.request_fingerprint.clone(),
            provider_name: self.provider_name.clone(),
            model_name: self.root_config.agent.model.clone(),
            effect: ApprovalMode::Allow,
            scope: sigil_kernel::HostedAuthorizationScope::ProviderRequest,
        };
        let active = EgressOrderingCoordinator::new(self.recorder.clone(), None)
            .authorize_hosted_request(&authorization, reservation, &|| true)?
            .begin_request()?;
        let processor = Arc::new(AuthorizedHostedFinalizer {
            inner: HostedEvidenceFinalizer::new(crate::web_search_tool::current_rfc3339()),
            active: Mutex::new(Some(active)),
        });
        Ok(AgentHostedTurn {
            hosted_tools: vec![request],
            evidence_processor: processor,
        })
    }
}

fn provider_hosted_route_enabled(
    route: WebSearchRoute,
    capability: HostedWebSearchCapability,
    provider_name: &str,
) -> Result<bool> {
    if capability.custom_tool_compatibility == HostedCustomToolCompatibility::Supported {
        return Ok(true);
    }
    if route == WebSearchRoute::ProviderHosted {
        bail!(
            "provider-hosted web search for {provider_name} cannot be combined with Sigil custom tools"
        );
    }
    Ok(false)
}

fn provider_hosted_safe_destination(root: &RootConfig, provider_name: &str) -> Result<String> {
    let base_url = match crate::normalize_provider_name(provider_name) {
        crate::OPENAI_RESPONSES_PROVIDER_KEY => {
            crate::resolve_openai_responses_config(root)?.base_url
        }
        crate::OPENAI_COMPAT_PROVIDER_KEY => crate::resolve_openai_compat_config(root)?.base_url,
        crate::ANTHROPIC_PROVIDER_KEY => crate::resolve_anthropic_config(root)?.base_url,
        crate::GEMINI_PROVIDER_KEY => crate::resolve_gemini_config(root)?.base_url,
        crate::DEEPSEEK_PROVIDER_KEY => crate::resolve_deepseek_config(root)?.base_url,
        other => bail!("hosted web search has no configured destination for provider {other}"),
    };
    safe_provider_origin(&base_url)
}

fn safe_provider_origin(base_url: &str) -> Result<String> {
    let parsed = Url::parse(base_url).context("provider base URL must be absolute")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        bail!("provider base URL must use HTTP(S) without embedded credentials");
    }
    let host = parsed
        .host_str()
        .context("provider base URL must contain a host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let port = parsed
        .port_or_known_default()
        .context("provider base URL has no effective port")?;
    let default_port = match parsed.scheme() {
        "http" => 80,
        "https" => 443,
        _ => unreachable!("provider scheme was validated above"),
    };
    if port == default_port {
        Ok(format!("{}://{host}/", parsed.scheme()))
    } else {
        Ok(format!("{}://{host}:{port}/", parsed.scheme()))
    }
}

fn hosted_limits(root: &RootConfig, capability: HostedWebSearchCapability) -> HostedToolLimits {
    let max_uses = (capability.max_uses_enforcement != HostedConstraintEnforcement::Unsupported)
        .then_some(root.web.provider_hosted_max_uses_per_request);
    let (allowed_domains, blocked_domains) =
        if capability.domain_filter_enforcement == HostedConstraintEnforcement::Unsupported {
            (Vec::new(), Vec::new())
        } else {
            (
                root.web.allowed_domains.clone(),
                root.web.blocked_domains.clone(),
            )
        };
    HostedToolLimits {
        max_uses,
        allowed_domains,
        blocked_domains,
    }
}

struct AuthorizedHostedFinalizer {
    inner: HostedEvidenceFinalizer,
    active: Mutex<Option<ActiveHostedEgress>>,
}

#[async_trait]
impl HostedEvidenceProcessor for AuthorizedHostedFinalizer {
    async fn finalize(
        &self,
        context: HostedFinalizationContext,
        buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError> {
        let finalized = self.inner.finalize(context, buffer).await;
        let mut active = self
            .active
            .lock()
            .map_err(|_| HostedTurnError::FinalizationFailed)?
            .take()
            .ok_or(HostedTurnError::FinalizationFailed)?;
        match finalized {
            Ok(finalized) => {
                active
                    .charge_model_chunk(finalized.assistant_text.len() as u64)
                    .map_err(|_| HostedTurnError::FinalizationFailed)?;
                active
                    .finish(hosted_terminal_status(&finalized))
                    .map_err(|_| HostedTurnError::FinalizationFailed)?;
                Ok(finalized)
            }
            Err(error) => {
                active
                    .finish(HostedToolTerminalStatus::RequestFailed)
                    .map_err(|_| HostedTurnError::FinalizationFailed)?;
                Err(error)
            }
        }
    }
}

impl Drop for AuthorizedHostedFinalizer {
    fn drop(&mut self) {
        if let Ok(slot) = self.active.get_mut()
            && let Some(active) = slot.take()
        {
            let _ = active.finish(HostedToolTerminalStatus::RequestFailed);
        }
    }
}

fn sha256(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
#[path = "tests/hosted_web_search_tests.rs"]
mod tests;
