use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sigil_code_intel::context::{
    CodeContextBuilder, CodeContextHit, LspContextSnapshot, LspContextSnapshotStatus, RepoMapLite,
    RepoMapLiteOptions, RepoSourceFileRef,
};
use sigil_code_intel::service::{CodeDiagnostic, CodeLocation, CodeSymbol};
use sigil_kernel::{
    ContextBodyRef, ContextInclusionReason, ContextItem, ContextScoreComponent,
    ContextScoreComponentKind, ContextSensitivity, ContextSource, ContextTrustLevel,
    DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES, PluginHookContextItems, PluginHookContextOptions,
    PluginHookOutputEnvelope, PluginTrustDecision, RuntimeContextCandidates, TaskMemoryV1,
    estimate_context_token_cost, plugin_hook_output_context_items, task_memory_context_items,
    validate_context_render_snippet,
};

const REPO_CONTEXT_MAX_FILES_SCANNED: usize = 160;
const REPO_CONTEXT_MAX_ITEMS: usize = 3;
const REPO_CONTEXT_MAX_BYTES_PER_FILE: usize = 8 * 1024;
const SOURCE_CONTEXT_MAX_FILES_SCANNED: usize = 640;
const SOURCE_CONTEXT_MAX_INDEX_BYTES_PER_FILE: usize = 192 * 1024;
const REPO_CONTEXT_SNIPPET_MAX_BYTES: usize = 2 * 1024;
const LSP_CONTEXT_MAX_ITEMS: usize = 6;
const LSP_CONTEXT_MAX_ITEM_TOKENS: usize = 64;
const LSP_CONTEXT_MAX_TOTAL_TOKENS: usize = 192;
const LSP_CONTEXT_TIMEOUT_MS: u64 = 150;
const PLUGIN_CONTEXT_MAX_ITEMS: usize = 2;
const PLUGIN_CONTEXT_MAX_ITEM_TOKENS: usize = 128;
const PLUGIN_CONTEXT_MAX_TOTAL_TOKENS: usize = 192;
const PLUGIN_CONTEXT_TIMEOUT_MS: u64 = 50;
const MCP_RESOURCE_CONTEXT_MAX_ITEMS: usize = 4;
const MCP_RESOURCE_CONTEXT_MAX_ITEM_TOKENS: usize = 128;
const MCP_RESOURCE_CONTEXT_MAX_TOTAL_TOKENS: usize = 256;
const MCP_RESOURCE_CONTEXT_TIMEOUT_MS: u64 = 50;
const MCP_RESOURCE_CONTEXT_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone)]
struct RepoContextCandidate {
    score: f32,
    score_breakdown: Vec<ContextScoreComponent>,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct RepoCandidateScore {
    score: f32,
    score_breakdown: Vec<ContextScoreComponent>,
}

#[derive(Debug, Clone)]
pub struct WarmLspContextProvider {
    snapshot: Option<LspContextSnapshot>,
}

impl WarmLspContextProvider {
    #[must_use]
    pub fn new(snapshot: Option<LspContextSnapshot>) -> Self {
        Self { snapshot }
    }
}

/// Runtime adapter for bounded plugin hook output that may contribute Context V0 rows.
///
/// The provider only admits output for manifests whose current trust decision is `Trusted`.
/// Untrusted output is retained as excluded provenance and never gets a model-visible snippet.
#[derive(Debug, Clone)]
pub struct PluginHookContextProvider {
    output: PluginHookOutputEnvelope,
    options: PluginHookContextOptions,
    trust_decision: PluginTrustDecision,
}

impl PluginHookContextProvider {
    #[must_use]
    pub fn new(
        output: PluginHookOutputEnvelope,
        options: PluginHookContextOptions,
        trust_decision: PluginTrustDecision,
    ) -> Self {
        Self {
            output,
            options,
            trust_decision,
        }
    }
}

/// Bounded MCP resource text supplied by an already-approved MCP resource read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceContextItem {
    pub server_name: String,
    pub uri: String,
    pub media_type: Option<String>,
    pub content: String,
    pub egress_decision: Option<String>,
    pub redacted: bool,
}

impl McpResourceContextItem {
    #[must_use]
    pub fn new(
        server_name: impl Into<String>,
        uri: impl Into<String>,
        media_type: Option<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            uri: uri.into(),
            media_type,
            content: content.into(),
            egress_decision: None,
            redacted: false,
        }
    }

    #[must_use]
    pub fn with_egress_decision(mut self, egress_decision: impl Into<String>) -> Self {
        self.egress_decision = Some(egress_decision.into());
        self
    }

    #[must_use]
    pub fn redacted(mut self, redacted: bool) -> Self {
        self.redacted = redacted;
        self
    }
}

/// Runtime adapter for already-read MCP resources.
///
/// This provider does not call an MCP server. MCP resource discovery/read still flows through the
/// normal MCP tool permission, trust and egress path; this adapter only turns bounded text results
/// into Context V0 candidates and re-applies context hard caps.
#[derive(Debug, Clone)]
pub struct McpResourceContextProvider {
    resources: Vec<McpResourceContextItem>,
}

impl McpResourceContextProvider {
    #[must_use]
    pub fn new(resources: Vec<McpResourceContextItem>) -> Self {
        Self { resources }
    }
}

#[derive(Debug, Clone)]
struct LspContextScore {
    score: f32,
    score_breakdown: Vec<ContextScoreComponent>,
}

/// One request-local context collection pass.
#[derive(Debug, Clone, Copy)]
pub struct ContextSourceRequest<'a> {
    pub workspace_root: &'a Path,
    pub query: &'a str,
}

impl<'a> ContextSourceRequest<'a> {
    #[must_use]
    pub fn new(workspace_root: &'a Path, query: &'a str) -> Self {
        Self {
            workspace_root,
            query,
        }
    }
}

/// Hard-cap and trust declaration for one runtime context source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourcePolicy {
    pub max_items: usize,
    pub max_item_tokens: usize,
    pub max_total_tokens: usize,
    pub trust_level: ContextTrustLevel,
    pub default_sensitivity: ContextSensitivity,
    pub requires_egress_decision: bool,
    pub timeout_ms: u64,
    pub allow_blocking_io: bool,
}

impl ContextSourcePolicy {
    /// Validates that the provider declared the hard caps required by the default scheduler.
    ///
    /// # Errors
    ///
    /// Returns an error when a provider omits an item, token, or timeout cap.
    pub fn validate(&self, source_id: &str) -> Result<()> {
        if source_id.trim().is_empty() {
            bail!("context source provider id must not be empty");
        }
        if self.max_items == 0 {
            bail!("context source provider {source_id} must declare max_items");
        }
        if self.max_item_tokens == 0 {
            bail!("context source provider {source_id} must declare max_item_tokens");
        }
        if self.max_total_tokens == 0 {
            bail!("context source provider {source_id} must declare max_total_tokens");
        }
        if self.timeout_ms == 0 {
            bail!("context source provider {source_id} must declare timeout_ms");
        }
        Ok(())
    }
}

/// Runtime-owned adapter contract for bounded Context V0 sources.
pub trait ContextSourceProvider {
    fn source_id(&self) -> &'static str;
    fn source_kind(&self) -> ContextSource;
    fn policy(&self) -> ContextSourcePolicy;
    fn collect(&self, request: &ContextSourceRequest<'_>) -> Result<RuntimeContextCandidates>;
}

impl ContextSourceProvider for WarmLspContextProvider {
    fn source_id(&self) -> &'static str {
        "warm-lsp-context"
    }

    fn source_kind(&self) -> ContextSource {
        ContextSource::LspSymbol
    }

    fn policy(&self) -> ContextSourcePolicy {
        ContextSourcePolicy {
            max_items: LSP_CONTEXT_MAX_ITEMS,
            max_item_tokens: LSP_CONTEXT_MAX_ITEM_TOKENS,
            max_total_tokens: LSP_CONTEXT_MAX_TOTAL_TOKENS,
            trust_level: ContextTrustLevel::UntrustedRepositoryData,
            default_sensitivity: ContextSensitivity::Repository,
            requires_egress_decision: false,
            timeout_ms: LSP_CONTEXT_TIMEOUT_MS,
            allow_blocking_io: false,
        }
    }

    fn collect(&self, request: &ContextSourceRequest<'_>) -> Result<RuntimeContextCandidates> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(lsp_context_unavailable_candidates(
                "unavailable",
                "no warm lsp context snapshot",
            ));
        };
        match &snapshot.status {
            LspContextSnapshotStatus::Ready => Ok(lsp_context_candidates_from_snapshot(
                snapshot,
                request.query,
            )),
            LspContextSnapshotStatus::Unavailable { reason } => {
                Ok(lsp_context_unavailable_candidates("unavailable", reason))
            }
            LspContextSnapshotStatus::TimedOut { timeout_ms } => {
                Ok(lsp_context_unavailable_candidates(
                    "timeout",
                    &format!("warm lsp context timed out after {timeout_ms}ms"),
                ))
            }
        }
    }
}

impl ContextSourceProvider for PluginHookContextProvider {
    fn source_id(&self) -> &'static str {
        "plugin-hook-context"
    }

    fn source_kind(&self) -> ContextSource {
        ContextSource::ExtensionProvided
    }

    fn policy(&self) -> ContextSourcePolicy {
        ContextSourcePolicy {
            max_items: PLUGIN_CONTEXT_MAX_ITEMS,
            max_item_tokens: PLUGIN_CONTEXT_MAX_ITEM_TOKENS,
            max_total_tokens: PLUGIN_CONTEXT_MAX_TOTAL_TOKENS,
            trust_level: ContextTrustLevel::ExtensionProvided,
            default_sensitivity: ContextSensitivity::Repository,
            requires_egress_decision: false,
            timeout_ms: PLUGIN_CONTEXT_TIMEOUT_MS,
            allow_blocking_io: false,
        }
    }

    fn collect(&self, _request: &ContextSourceRequest<'_>) -> Result<RuntimeContextCandidates> {
        let mut context = plugin_hook_context_candidates(&self.output, self.options.clone())?;
        if self.trust_decision != PluginTrustDecision::Trusted {
            for item in &mut context.items {
                item.inclusion_reason = ContextInclusionReason::ExcludedUntrustedWorkspace;
            }
            context.snippets.clear();
        }
        Ok(context)
    }
}

impl ContextSourceProvider for McpResourceContextProvider {
    fn source_id(&self) -> &'static str {
        "mcp-resource-context"
    }

    fn source_kind(&self) -> ContextSource {
        ContextSource::McpResource
    }

    fn policy(&self) -> ContextSourcePolicy {
        ContextSourcePolicy {
            max_items: MCP_RESOURCE_CONTEXT_MAX_ITEMS,
            max_item_tokens: MCP_RESOURCE_CONTEXT_MAX_ITEM_TOKENS,
            max_total_tokens: MCP_RESOURCE_CONTEXT_MAX_TOTAL_TOKENS,
            trust_level: ContextTrustLevel::ToolObservation,
            default_sensitivity: ContextSensitivity::External,
            requires_egress_decision: true,
            timeout_ms: MCP_RESOURCE_CONTEXT_TIMEOUT_MS,
            allow_blocking_io: false,
        }
    }

    fn collect(&self, _request: &ContextSourceRequest<'_>) -> Result<RuntimeContextCandidates> {
        Ok(mcp_resource_context_candidates(&self.resources))
    }
}

/// Collects and hard-caps all configured context source providers for one request.
///
/// # Errors
///
/// Returns an error only when a provider violates the scheduler contract by omitting hard caps.
/// Provider collection errors and panics are converted into excluded provenance rows so ordinary
/// request assembly can continue.
pub fn collect_context_from_source_providers(
    providers: &[&dyn ContextSourceProvider],
    request: &ContextSourceRequest<'_>,
) -> Result<RuntimeContextCandidates> {
    let mut collected = RuntimeContextCandidates::new();
    for provider in providers {
        collected.extend(collect_context_from_source_provider(*provider, request)?);
    }
    Ok(collected)
}

/// Collects and hard-caps one context source provider for the default scheduler.
///
/// # Errors
///
/// Returns an error when the provider policy does not declare required hard caps.
pub fn collect_context_from_source_provider(
    provider: &dyn ContextSourceProvider,
    request: &ContextSourceRequest<'_>,
) -> Result<RuntimeContextCandidates> {
    let source_id = provider.source_id();
    let source_kind = provider.source_kind();
    let policy = provider.policy();
    policy.validate(source_id)?;

    let result = catch_unwind(AssertUnwindSafe(|| provider.collect(request)));
    match result {
        Ok(Ok(candidates)) => enforce_context_source_policy(source_kind, &policy, candidates),
        Ok(Err(error)) => Ok(context_source_failure_candidates(
            source_id,
            source_kind,
            &policy,
            &format!("{error:#}"),
        )),
        Err(_) => Ok(context_source_failure_candidates(
            source_id,
            source_kind,
            &policy,
            "context source provider panicked",
        )),
    }
}

/// Converts typed task memory into runtime context candidates with provenance preserved.
///
/// This is intentionally a thin runtime boundary over the kernel adapter: runtime callers can
/// assemble context without depending on TUI-specific rendering, while the trust/sensitivity
/// invariants remain enforced by kernel types.
pub fn context_items_from_task_memory(memory: &TaskMemoryV1) -> Result<Vec<ContextItem>> {
    task_memory_context_items(memory)
}

/// Converts trusted plugin hook output into runtime context candidates with provenance preserved.
///
/// Runtime callers must still decide when to execute hooks and whether to pass the resulting items
/// into prompt assembly. The helper never creates verification evidence or task-memory facts.
pub fn context_items_from_plugin_hook_output(
    output: &PluginHookOutputEnvelope,
    options: PluginHookContextOptions,
) -> Result<PluginHookContextItems> {
    plugin_hook_output_context_items(output, options)
}

fn plugin_hook_context_candidates(
    output: &PluginHookOutputEnvelope,
    options: PluginHookContextOptions,
) -> Result<RuntimeContextCandidates> {
    let context = plugin_hook_output_context_items(output, options)?;
    Ok(RuntimeContextCandidates {
        items: context.items,
        snippets: context.snippets,
    })
}

fn mcp_resource_context_candidates(
    resources: &[McpResourceContextItem],
) -> RuntimeContextCandidates {
    let mut context = RuntimeContextCandidates::new();
    for (index, resource) in resources.iter().enumerate() {
        let media_type_allowed = mcp_resource_media_type_allowed(resource.media_type.as_deref());
        let bounded_content =
            truncate_to_char_boundary(&resource.content, MCP_RESOURCE_CONTEXT_MAX_BYTES).to_owned();
        let body = if media_type_allowed {
            bounded_content
        } else {
            format!(
                "MCP resource {} omitted because media type {} is unsupported",
                resource.uri,
                resource.media_type.as_deref().unwrap_or("unknown")
            )
        };
        let id = format!(
            "mcp-resource:{}:{}:{}",
            index,
            context_source_item_id_segment(&resource.server_name),
            context_source_item_id_segment(&resource.uri)
        );
        let mut sensitivity = ContextSensitivity::External;
        if resource.redacted {
            sensitivity = ContextSensitivity::PotentialSecret;
        }
        let inclusion_reason = if media_type_allowed {
            ContextInclusionReason::RetrievalHit
        } else {
            ContextInclusionReason::ExcludedUnsupported
        };
        let item = ContextItem {
            id: id.clone(),
            source: ContextSource::McpResource,
            source_event_id: Some(format!("mcp:{}:{}", resource.server_name, resource.uri)),
            trust_level: ContextTrustLevel::ToolObservation,
            sensitivity,
            egress_decision: resource.egress_decision.clone(),
            repo_revision: None,
            token_cost: estimate_context_token_cost(&body),
            score: Some(0.45),
            score_breakdown: vec![score_component(
                ContextScoreComponentKind::RetrievalScore,
                45.0,
            )],
            inclusion_reason,
            body_ref: ContextBodyRef::inline(&body),
        };
        if media_type_allowed {
            context.snippets.insert(id, body);
        }
        context.items.push(item);
    }
    context
}

fn mcp_resource_media_type_allowed(media_type: Option<&str>) -> bool {
    let Some(media_type) = media_type else {
        return true;
    };
    let normalized = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    normalized.is_empty()
        || normalized.starts_with("text/")
        || matches!(
            normalized.as_str(),
            "application/json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/markdown"
        )
}

/// Builds bounded repository-file Context V0 candidates from a user query.
///
/// This is intentionally conservative: it never leaves the workspace root, avoids common generated
/// and local-development directories, and emits excluded metadata instead of reading secret-like
/// files. It is a production wiring point for Context V0, not a persistent repo index.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized. Individual file read failures
/// are ignored so request assembly degrades gracefully.
pub fn context_candidates_from_repo_query(
    workspace_root: &Path,
    query: &str,
) -> Result<RuntimeContextCandidates> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let query = query.trim();
    if query.is_empty() {
        return Ok(RuntimeContextCandidates::default());
    }

    let mut candidates = BTreeMap::<PathBuf, RepoContextCandidate>::new();
    for path in explicit_query_paths(query) {
        if let Some(relative) = normalize_workspace_relative_path(&workspace_root, &path) {
            upsert_context_candidate(
                &mut candidates,
                relative,
                100.0,
                vec![score_component(
                    ContextScoreComponentKind::ExplicitPath,
                    100.0,
                )],
                ContextInclusionReason::RetrievalHit,
                BTreeSet::new(),
            );
        }
    }

    let explicit_candidate_count = candidates.len();
    let terms = lexical_query_terms(query);
    if explicit_candidate_count == 0 {
        if !terms.is_empty() {
            collect_lexical_file_candidates(&workspace_root, &terms, &mut candidates);
        }
        if !terms.is_empty()
            || query_has_source_intent(query)
            || !explicit_code_query_terms(query).is_empty()
        {
            collect_source_symbol_candidates(&workspace_root, query, &terms, &mut candidates);
        }
    }

    let builder = CodeContextBuilder::new();
    let mut runtime = RuntimeContextCandidates::default();
    let mut ranked = candidates.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .score
            .partial_cmp(&left.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (relative_path, candidate) in ranked.into_iter().take(REPO_CONTEXT_MAX_ITEMS) {
        let hit = repo_context_hit(&workspace_root, &builder, relative_path, candidate);
        runtime
            .snippets
            .insert(hit.item.id.clone(), hit.snippet.clone());
        runtime.items.push(hit.item);
    }
    Ok(runtime)
}

/// Builds bounded Context V0 candidates from safe request-local sources.
///
/// Repository context is collected synchronously with existing bounded scans. LSP context is only
/// attached from a caller-supplied warm snapshot; this function never starts or queries a language
/// server during prompt assembly.
///
/// # Errors
///
/// Returns an error when the repository context helper cannot canonicalize the workspace root or
/// when a context source provider violates the hard-cap contract.
pub fn context_candidates_from_safe_sources(
    workspace_root: &Path,
    query: &str,
    lsp_snapshot: Option<&LspContextSnapshot>,
) -> Result<RuntimeContextCandidates> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(RuntimeContextCandidates::default());
    }

    let mut context = context_candidates_from_repo_query(workspace_root, query)?;
    let lsp_provider = WarmLspContextProvider::new(lsp_snapshot.cloned());
    context.extend(collect_context_from_source_provider(
        &lsp_provider,
        &ContextSourceRequest::new(workspace_root, query),
    )?);
    Ok(context)
}

fn enforce_context_source_policy(
    source_kind: ContextSource,
    policy: &ContextSourcePolicy,
    mut candidates: RuntimeContextCandidates,
) -> Result<RuntimeContextCandidates> {
    let mut capped = RuntimeContextCandidates::new();
    let mut included_count = 0usize;
    let mut used_tokens = 0usize;

    for mut item in candidates.items {
        let mut snippet = candidates.snippets.remove(&item.id);
        normalize_context_source_item(source_kind.clone(), policy, &mut item);

        if item.inclusion_reason.is_included()
            && !context_source_provider_accepts_item(&source_kind, &item.source)
        {
            item.inclusion_reason = ContextInclusionReason::ExcludedUnsupported;
            snippet = None;
        }
        if item.inclusion_reason.is_included() && item.token_cost > policy.max_item_tokens {
            item.inclusion_reason = ContextInclusionReason::ExcludedTokenBudget;
            snippet = None;
        }
        if item.inclusion_reason.is_included() && included_count >= policy.max_items {
            item.inclusion_reason = ContextInclusionReason::ExcludedTokenBudget;
            snippet = None;
        }
        if item.inclusion_reason.is_included()
            && item.token_cost > policy.max_total_tokens.saturating_sub(used_tokens)
        {
            item.inclusion_reason = ContextInclusionReason::ExcludedTokenBudget;
            snippet = None;
        }
        if item.inclusion_reason.is_included()
            && let Some(candidate_snippet) = snippet.as_deref()
            && validate_context_render_snippet(
                &item,
                candidate_snippet,
                DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES
                    .min(policy.max_item_tokens.saturating_mul(512)),
            )
            .is_err()
        {
            item.inclusion_reason = ContextInclusionReason::ExcludedUnsupported;
            snippet = None;
        }
        if item.inclusion_reason.is_included() && item.validate().is_err() {
            item.inclusion_reason = ContextInclusionReason::ExcludedUnsupported;
            snippet = None;
        }

        if item.inclusion_reason.is_included() {
            included_count = included_count.saturating_add(1);
            used_tokens = used_tokens.saturating_add(item.token_cost);
            if let Some(snippet) = snippet {
                capped.snippets.insert(item.id.clone(), snippet);
            }
        }
        capped.items.push(item);
    }

    capped.items.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(capped)
}

fn context_source_provider_accepts_item(
    provider_source: &ContextSource,
    item_source: &ContextSource,
) -> bool {
    provider_source == item_source
        || matches!(
            (provider_source, item_source),
            (
                ContextSource::LspSymbol,
                ContextSource::LspDiagnostic | ContextSource::LspReference
            )
        )
}

fn normalize_context_source_item(
    source_kind: ContextSource,
    policy: &ContextSourcePolicy,
    item: &mut ContextItem,
) {
    if item.source == source_kind {
        item.trust_level = policy.trust_level;
    }
    if item.sensitivity < policy.default_sensitivity {
        item.sensitivity = policy.default_sensitivity;
    }
    if item.inclusion_reason.is_included()
        && policy.requires_egress_decision
        && item.egress_decision.is_none()
    {
        item.inclusion_reason = ContextInclusionReason::ExcludedEgressDenied;
    }
}

fn context_source_failure_candidates(
    source_id: &str,
    source_kind: ContextSource,
    policy: &ContextSourcePolicy,
    message: &str,
) -> RuntimeContextCandidates {
    let body = format!("context source {} unavailable: {message}", source_id.trim());
    let item = ContextItem {
        id: format!(
            "context-source:{}:failure",
            context_source_item_id_segment(source_id)
        ),
        source: source_kind,
        source_event_id: None,
        trust_level: policy.trust_level,
        sensitivity: policy.default_sensitivity,
        egress_decision: None,
        repo_revision: None,
        token_cost: estimate_context_token_cost(&body),
        score: None,
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::ExcludedUnsupported,
        body_ref: ContextBodyRef::inline(&body),
    };
    RuntimeContextCandidates {
        items: vec![item],
        snippets: BTreeMap::new(),
    }
}

fn context_source_item_id_segment(source_id: &str) -> String {
    let mut segment = source_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    segment.truncate(96);
    if segment.is_empty() {
        "unknown".to_owned()
    } else {
        segment
    }
}

fn lsp_context_candidates_from_snapshot(
    snapshot: &LspContextSnapshot,
    query: &str,
) -> RuntimeContextCandidates {
    let profile = LspContextQueryProfile::from_query(query);
    let builder = CodeContextBuilder::new();
    let mut runtime = RuntimeContextCandidates::new();

    for symbol in &snapshot.symbols {
        let Some(score) = lsp_symbol_score(symbol, &profile) else {
            continue;
        };
        push_lsp_hit(&mut runtime, builder.symbol_hit(symbol), score);
    }
    for diagnostic in &snapshot.diagnostics {
        let Some(score) = lsp_diagnostic_score(diagnostic, &profile) else {
            continue;
        };
        push_lsp_hit(&mut runtime, builder.diagnostic_hit(diagnostic), score);
    }
    for reference in &snapshot.references {
        let Some(score) = lsp_reference_score(reference, &profile) else {
            continue;
        };
        push_lsp_hit(&mut runtime, builder.reference_hit(reference), score);
    }

    if runtime.items.is_empty() {
        return lsp_context_unavailable_candidates(
            "miss",
            "warm lsp context cache had no query-relevant rows",
        );
    }

    runtime.items.sort_by(|left, right| {
        right
            .score
            .unwrap_or_default()
            .partial_cmp(&left.score.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    runtime
}

fn push_lsp_hit(
    runtime: &mut RuntimeContextCandidates,
    mut hit: CodeContextHit,
    score: LspContextScore,
) {
    hit.item.score = Some(score.score);
    hit.item.score_breakdown = score.score_breakdown;
    hit.item.inclusion_reason = ContextInclusionReason::WarmLspMatch;
    runtime
        .snippets
        .insert(hit.item.id.clone(), hit.snippet.clone());
    runtime.items.push(hit.item);
}

fn lsp_context_unavailable_candidates(kind: &str, reason: &str) -> RuntimeContextCandidates {
    let body = format!("warm lsp context {kind}: {}", reason.trim());
    let item = ContextItem {
        id: format!("lsp-context:{kind}"),
        source: ContextSource::LspSymbol,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: None,
        token_cost: estimate_context_token_cost(&body),
        score: None,
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::ExcludedUnsupported,
        body_ref: ContextBodyRef::inline(&body),
    };
    RuntimeContextCandidates {
        items: vec![item],
        snippets: BTreeMap::new(),
    }
}

#[derive(Debug, Clone)]
struct LspContextQueryProfile {
    source_intent: bool,
    diagnostic_intent: bool,
    reference_intent: bool,
    lexical_terms: BTreeSet<String>,
    symbol_terms: BTreeSet<String>,
}

impl LspContextQueryProfile {
    fn from_query(query: &str) -> Self {
        let lexical_terms = lexical_query_terms(query);
        let source_profile = SourceQueryProfile::from_query(query, &lexical_terms);
        let lower = query.to_ascii_lowercase();
        Self {
            source_intent: source_profile.source_intent,
            diagnostic_intent: contains_any(
                &lower,
                &[
                    "diagnostic",
                    "diagnostics",
                    "error",
                    "warning",
                    "报错",
                    "诊断",
                ],
            ),
            reference_intent: contains_any(
                &lower,
                &["reference", "references", "usage", "usages", "调用", "引用"],
            ),
            lexical_terms: source_profile.lexical_terms,
            symbol_terms: source_profile.symbol_terms,
        }
    }
}

fn lsp_symbol_score(
    symbol: &CodeSymbol,
    profile: &LspContextQueryProfile,
) -> Option<LspContextScore> {
    let name = symbol.name.to_ascii_lowercase();
    let path = symbol.path.to_ascii_lowercase();
    let container = symbol
        .container_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut score_breakdown = Vec::new();

    for term in &profile.symbol_terms {
        if source_term_variants(&name)
            .iter()
            .any(|variant| variant == term)
            || name == *term
        {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                120.0,
            );
        } else if name.contains(term) || container.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                70.0,
            );
        }
        if path.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                44.0,
            );
        }
    }

    for term in &profile.lexical_terms {
        if path.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                16.0,
            );
        }
        if name.contains(term) || container.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::RetrievalScore,
                10.0,
            );
        }
    }

    if profile.source_intent && !score_breakdown.is_empty() {
        push_score_component(
            &mut score_breakdown,
            ContextScoreComponentKind::RetrievalScore,
            8.0,
        );
    }

    let score = score_from_breakdown(&score_breakdown);
    (score >= 10.0).then_some(LspContextScore {
        score,
        score_breakdown,
    })
}

fn lsp_diagnostic_score(
    diagnostic: &CodeDiagnostic,
    profile: &LspContextQueryProfile,
) -> Option<LspContextScore> {
    if !profile.diagnostic_intent && !profile.source_intent {
        return None;
    }

    let path = diagnostic.path.to_ascii_lowercase();
    let message = diagnostic.message.to_ascii_lowercase();
    let severity = diagnostic.severity.to_ascii_lowercase();
    let mut score_breakdown = Vec::new();

    if profile.diagnostic_intent {
        push_score_component(
            &mut score_breakdown,
            ContextScoreComponentKind::RetrievalScore,
            18.0,
        );
    }
    for term in profile
        .lexical_terms
        .iter()
        .chain(profile.symbol_terms.iter())
    {
        if path.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                22.0,
            );
        }
        if message.contains(term) || severity.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::RetrievalScore,
                10.0,
            );
        }
    }

    let score = score_from_breakdown(&score_breakdown);
    (score >= 18.0).then_some(LspContextScore {
        score,
        score_breakdown,
    })
}

fn lsp_reference_score(
    location: &CodeLocation,
    profile: &LspContextQueryProfile,
) -> Option<LspContextScore> {
    if !profile.reference_intent && !profile.source_intent && profile.symbol_terms.is_empty() {
        return None;
    }

    let path = location.path.to_ascii_lowercase();
    let preview = location
        .preview
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut score_breakdown = Vec::new();

    if profile.reference_intent {
        push_score_component(
            &mut score_breakdown,
            ContextScoreComponentKind::RetrievalScore,
            16.0,
        );
    }
    for term in profile
        .lexical_terms
        .iter()
        .chain(profile.symbol_terms.iter())
    {
        if path.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                20.0,
            );
        }
        if preview.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::RetrievalScore,
                14.0,
            );
        }
    }

    let score = score_from_breakdown(&score_breakdown);
    (score >= 16.0).then_some(LspContextScore {
        score,
        score_breakdown,
    })
}

fn collect_lexical_file_candidates(
    workspace_root: &Path,
    terms: &BTreeSet<String>,
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
) {
    let mut stack = vec![workspace_root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !should_skip_repo_context_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            if !file_type.is_file() || scanned >= REPO_CONTEXT_MAX_FILES_SCANNED {
                continue;
            }
            scanned = scanned.saturating_add(1);
            let Ok(relative) = path.strip_prefix(workspace_root) else {
                continue;
            };
            if should_skip_repo_context_path(relative) {
                continue;
            }
            if let Some(scored) = lexical_file_score(&path, relative, terms) {
                upsert_context_candidate(
                    candidates,
                    relative.to_path_buf(),
                    scored.score,
                    scored.score_breakdown,
                    ContextInclusionReason::RetrievalHit,
                    BTreeSet::new(),
                );
            }
        }
        if scanned >= REPO_CONTEXT_MAX_FILES_SCANNED {
            break;
        }
    }
}

fn collect_source_symbol_candidates(
    workspace_root: &Path,
    query: &str,
    lexical_terms: &BTreeSet<String>,
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
) {
    let profile = SourceQueryProfile::from_query(query, lexical_terms);
    if !profile.should_scan_sources() {
        return;
    }

    let Ok(repo_map) = sigil_code_intel::build_repo_map_lite(
        workspace_root,
        RepoMapLiteOptions {
            max_files_scanned: SOURCE_CONTEXT_MAX_FILES_SCANNED,
            max_index_bytes_per_file: SOURCE_CONTEXT_MAX_INDEX_BYTES_PER_FILE,
        },
    ) else {
        return;
    };

    for source_file in &repo_map.source_files {
        let Some(scored) = source_symbol_file_score(source_file, &repo_map, &profile) else {
            continue;
        };
        upsert_context_candidate(
            candidates,
            source_file.path.clone(),
            scored.score,
            scored.score_breakdown,
            scored.inclusion_reason,
            scored.snippet_terms,
        );
    }
}

fn upsert_context_candidate(
    candidates: &mut BTreeMap<PathBuf, RepoContextCandidate>,
    path: PathBuf,
    score: f32,
    score_breakdown: Vec<ContextScoreComponent>,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
) {
    candidates
        .entry(path)
        .and_modify(|existing| {
            if score > existing.score {
                existing.score = score;
                existing.score_breakdown = score_breakdown.clone();
                existing.inclusion_reason = inclusion_reason.clone();
                existing.snippet_terms = snippet_terms.clone();
            } else if score == existing.score && existing.snippet_terms.is_empty() {
                existing.snippet_terms = snippet_terms.clone();
            }
        })
        .or_insert(RepoContextCandidate {
            score,
            score_breakdown,
            inclusion_reason,
            snippet_terms,
        });
}

fn repo_context_hit(
    workspace_root: &Path,
    builder: &CodeContextBuilder,
    relative_path: PathBuf,
    candidate: RepoContextCandidate,
) -> CodeContextHit {
    if is_secret_like_path(&relative_path) {
        let mut hit = builder
            .clone()
            .sensitivity(ContextSensitivity::Secret)
            .repo_file_hit(
                relative_path,
                "secret-like repository file omitted from automatic context",
            );
        hit.item.score = Some(candidate.score);
        hit.item.score_breakdown = candidate.score_breakdown;
        return hit;
    }

    let full_path = workspace_root.join(&relative_path);
    let body =
        read_repo_context_snippet_for_candidate(&full_path, &candidate).unwrap_or_else(|| {
            format!(
                "repository file {} could not be read for automatic context",
                relative_path.display()
            )
        });
    let mut hit = builder.repo_file_hit(relative_path, body);
    hit.item.score = Some(candidate.score);
    hit.item.score_breakdown = candidate.score_breakdown;
    hit.item.inclusion_reason = candidate.inclusion_reason;
    hit
}

fn read_repo_context_snippet(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let indexed_len = bytes.len().min(REPO_CONTEXT_MAX_BYTES_PER_FILE);
    let indexed = &bytes[..indexed_len];
    if indexed.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(indexed).ok()?;
    Some(truncate_to_char_boundary(text, REPO_CONTEXT_SNIPPET_MAX_BYTES).to_owned())
}

fn read_repo_context_snippet_for_candidate(
    path: &Path,
    candidate: &RepoContextCandidate,
) -> Option<String> {
    if !candidate.snippet_terms.is_empty()
        && let Some(snippet) =
            read_repo_context_snippet_around_terms(path, &candidate.snippet_terms)
    {
        return Some(snippet);
    }
    read_repo_context_snippet(path)
}

fn read_repo_context_snippet_around_terms(path: &Path, terms: &BTreeSet<String>) -> Option<String> {
    let text = read_repo_context_index(path)?;
    let lower_text = text.to_ascii_lowercase();
    let mut ranked_terms = terms.iter().collect::<Vec<_>>();
    ranked_terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let position = ranked_terms
        .into_iter()
        .find_map(|term| lower_text.find(term.as_str()))?;
    Some(snippet_window_around_byte(&text, position, REPO_CONTEXT_SNIPPET_MAX_BYTES).to_owned())
}

fn lexical_file_score(
    path: &Path,
    relative: &Path,
    terms: &BTreeSet<String>,
) -> Option<RepoCandidateScore> {
    let relative_text = relative.to_string_lossy().to_lowercase();
    let mut score_breakdown = Vec::new();
    let path_score = terms
        .iter()
        .filter(|term| relative_text.contains(term.as_str()))
        .count() as f32
        * 10.0;
    push_score_component(
        &mut score_breakdown,
        ContextScoreComponentKind::RetrievalScore,
        path_score,
    );

    if path_score == 0.0 && !looks_like_text_file(relative) {
        return None;
    }

    if let Some(snippet) = read_repo_context_snippet(path) {
        let text = snippet.to_lowercase();
        let content_score = terms
            .iter()
            .filter(|term| text.contains(term.as_str()))
            .count() as f32;
        push_score_component(
            &mut score_breakdown,
            ContextScoreComponentKind::RetrievalScore,
            content_score,
        );
    }
    let score = score_from_breakdown(&score_breakdown);
    (score > 0.0).then_some(RepoCandidateScore {
        score,
        score_breakdown,
    })
}

#[derive(Debug, Clone)]
struct SourceQueryProfile {
    source_intent: bool,
    lexical_terms: BTreeSet<String>,
    symbol_terms: BTreeSet<String>,
}

impl SourceQueryProfile {
    fn from_query(query: &str, lexical_terms: &BTreeSet<String>) -> Self {
        let mut source_intent = query_has_source_intent(query);
        let mut source_terms = BTreeSet::new();
        let mut symbol_terms = BTreeSet::new();

        for term in explicit_code_query_terms(query) {
            if is_path_like_query_term(&term) {
                source_terms.insert(term.to_ascii_lowercase());
            } else {
                for variant in source_term_variants(&term) {
                    symbol_terms.insert(variant.clone());
                    source_terms.insert(variant);
                }
            }
        }

        for term in lexical_terms {
            match source_query_term_role(term) {
                SourceQueryTermRole::SourceIntentHint => {
                    source_intent = true;
                }
                SourceQueryTermRole::SymbolLike => {
                    for variant in source_term_variants(term) {
                        symbol_terms.insert(variant.clone());
                        source_terms.insert(variant);
                    }
                }
                SourceQueryTermRole::PathLike | SourceQueryTermRole::LexicalHint => {
                    source_terms.insert(term.clone());
                }
                SourceQueryTermRole::NaturalLanguage => {}
            }
        }

        for token in source_query_tokens(query) {
            match source_query_term_role(&token) {
                SourceQueryTermRole::SourceIntentHint => {
                    source_intent = true;
                }
                SourceQueryTermRole::SymbolLike | SourceQueryTermRole::PathLike => {
                    for variant in source_term_variants(&token) {
                        symbol_terms.insert(variant.clone());
                        source_terms.insert(variant);
                    }
                }
                SourceQueryTermRole::LexicalHint => {
                    source_terms.insert(token.to_ascii_lowercase());
                }
                SourceQueryTermRole::NaturalLanguage => {}
            }
        }

        Self {
            source_intent,
            lexical_terms: source_terms,
            symbol_terms,
        }
    }

    fn should_scan_sources(&self) -> bool {
        self.source_intent || !self.symbol_terms.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceQueryTermRole {
    SourceIntentHint,
    SymbolLike,
    PathLike,
    LexicalHint,
    NaturalLanguage,
}

fn source_query_term_role(term: &str) -> SourceQueryTermRole {
    let trimmed = term.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''));
    if trimmed.is_empty() {
        return SourceQueryTermRole::NaturalLanguage;
    }

    let lower = trimmed.to_ascii_lowercase();
    if is_path_like_query_term(trimmed) {
        return SourceQueryTermRole::PathLike;
    }
    if is_source_intent_hint(&lower) {
        return SourceQueryTermRole::SourceIntentHint;
    }
    if is_natural_language_query_term(&lower) {
        return SourceQueryTermRole::NaturalLanguage;
    }
    if is_code_like_query_token(trimmed) {
        return SourceQueryTermRole::SymbolLike;
    }
    if lower.len() >= 4 {
        return SourceQueryTermRole::LexicalHint;
    }

    SourceQueryTermRole::NaturalLanguage
}

#[derive(Debug, Clone)]
struct SourceSymbolScore {
    score: f32,
    score_breakdown: Vec<ContextScoreComponent>,
    inclusion_reason: ContextInclusionReason,
    snippet_terms: BTreeSet<String>,
}

fn source_symbol_file_score(
    source_file: &RepoSourceFileRef,
    repo_map: &RepoMapLite,
    profile: &SourceQueryProfile,
) -> Option<SourceSymbolScore> {
    let relative = &source_file.path;
    let relative_text = relative.to_string_lossy().to_ascii_lowercase();
    let file_stem = relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let index_text = source_file.indexed_text.to_ascii_lowercase();
    let mut score_breakdown = Vec::new();
    if profile.source_intent {
        push_score_component(
            &mut score_breakdown,
            ContextScoreComponentKind::SourcePath,
            28.0,
        );
    }
    let mut matched_symbol = false;
    let mut snippet_terms = BTreeSet::new();
    let symbols = repo_map
        .symbols
        .iter()
        .filter(|symbol| symbol.path == *relative)
        .collect::<Vec<_>>();

    for term in &profile.symbol_terms {
        if file_stem == *term {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                130.0,
            );
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        } else if file_stem.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                85.0,
            );
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
        if relative_text.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                70.0,
            );
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
        if index_text.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                95.0,
            );
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
        if symbols.iter().any(|symbol| {
            source_term_variants(&symbol.name)
                .iter()
                .any(|variant| variant == term)
                || symbol.name.to_ascii_lowercase().contains(term)
        }) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::ExactSymbol,
                150.0,
            );
            matched_symbol = true;
            snippet_terms.insert(term.clone());
        }
    }

    for term in &profile.lexical_terms {
        if relative_text.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::SourcePath,
                18.0,
            );
            snippet_terms.insert(term.clone());
        }
        if index_text.contains(term) {
            push_score_component(
                &mut score_breakdown,
                ContextScoreComponentKind::RetrievalScore,
                4.0,
            );
            snippet_terms.insert(term.clone());
        }
    }

    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value == "tests")
    }) {
        for component in &mut score_breakdown {
            component.value *= 0.45;
        }
    }

    let score = score_from_breakdown(&score_breakdown);
    if score < 36.0 {
        return None;
    }

    let inclusion_reason = if matched_symbol {
        ContextInclusionReason::ExactSymbolMatch
    } else {
        ContextInclusionReason::SourcePathMatch
    };
    Some(SourceSymbolScore {
        score,
        score_breakdown,
        inclusion_reason,
        snippet_terms,
    })
}

fn score_component(kind: ContextScoreComponentKind, value: f32) -> ContextScoreComponent {
    ContextScoreComponent { kind, value }
}

fn push_score_component(
    breakdown: &mut Vec<ContextScoreComponent>,
    kind: ContextScoreComponentKind,
    value: f32,
) {
    if value == 0.0 {
        return;
    }
    if let Some(component) = breakdown
        .iter_mut()
        .find(|component| component.kind == kind)
    {
        component.value += value;
    } else {
        breakdown.push(score_component(kind, value));
    }
}

fn score_from_breakdown(breakdown: &[ContextScoreComponent]) -> f32 {
    breakdown.iter().map(|component| component.value).sum()
}

fn read_repo_context_index(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let indexed_len = bytes.len().min(SOURCE_CONTEXT_MAX_INDEX_BYTES_PER_FILE);
    let indexed = &bytes[..indexed_len];
    if indexed.contains(&0) {
        return None;
    }
    std::str::from_utf8(indexed).ok().map(str::to_owned)
}

fn query_has_source_intent(query: &str) -> bool {
    let lower = query.to_lowercase();
    contains_any(
        &lower,
        &[
            "rust",
            "source",
            "source file",
            "源码",
            "源码文件",
            "函数",
            "trait",
            "function",
            "definition",
            "defined",
            "module",
            "模块",
            "runner",
            "handoff",
            "surface",
            "在哪个",
            "定义在哪",
        ],
    )
}

fn source_query_tokens(query: &str) -> impl Iterator<Item = String> + '_ {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_owned)
}

fn explicit_code_query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    collect_delimited_query_terms(query, '`', &mut terms);
    collect_delimited_query_terms(query, '"', &mut terms);
    collect_delimited_query_terms(query, '\'', &mut terms);
    terms
}

fn collect_delimited_query_terms(query: &str, delimiter: char, terms: &mut BTreeSet<String>) {
    let mut rest = query;
    while let Some(start) = rest.find(delimiter) {
        rest = &rest[start + delimiter.len_utf8()..];
        let Some(end) = rest.find(delimiter) else {
            break;
        };
        collect_explicit_query_term(&rest[..end], terms);
        rest = &rest[end + delimiter.len_utf8()..];
    }
}

fn collect_explicit_query_term(segment: &str, terms: &mut BTreeSet<String>) {
    let term = segment
        .trim()
        .trim_matches(|ch: char| matches!(ch, '`' | '"' | '\'' | '.' | ',' | ':' | ';'));
    if term.len() < 3 || term.len() > 96 || term.chars().any(char::is_whitespace) {
        return;
    }
    if term.chars().any(|ch| ch.is_ascii_alphanumeric()) {
        terms.insert(term.to_owned());
    }
}

fn is_code_like_query_token(token: &str) -> bool {
    token.contains('_')
        || token.contains('-')
        || token.chars().any(char::is_uppercase) && token.chars().any(char::is_lowercase)
}

fn is_source_intent_hint(term: &str) -> bool {
    matches!(
        term,
        "rust"
            | "source"
            | "source-file"
            | "repo-file"
            | "file"
            | "files"
            | "implementation"
            | "implementations"
            | "implements"
            | "defined"
            | "definition"
            | "function"
            | "functions"
            | "trait"
            | "traits"
            | "module"
            | "modules"
    )
}

fn is_natural_language_query_term(term: &str) -> bool {
    matches!(
        term,
        "which"
            | "where"
            | "what"
            | "who"
            | "when"
            | "why"
            | "how"
            | "only"
            | "output"
            | "answer"
            | "provided"
            | "automatic"
            | "system"
            | "most"
            | "likely"
            | "please"
            | "based"
            | "using"
            | "without"
            | "with"
            | "from"
            | "into"
            | "this"
            | "that"
            | "the"
            | "and"
            | "for"
            | "are"
            | "you"
            | "not"
    )
}

fn is_path_like_query_term(term: &str) -> bool {
    term.contains('/')
        || term.contains('\\')
        || term.ends_with(".rs")
        || term.ends_with(".toml")
        || term.ends_with(".md")
}

fn source_term_variants(token: &str) -> BTreeSet<String> {
    let mut variants = BTreeSet::new();
    let trimmed = token.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\''));
    if trimmed.is_empty() {
        return variants;
    }
    let lower = trimmed.to_ascii_lowercase();
    variants.insert(lower.clone());
    variants.insert(lower.replace('-', "_"));
    variants.insert(lower.replace(['_', '-'], " "));
    variants.insert(lower.replace(['_', '-'], ""));
    let snake = camel_to_snake(trimmed);
    if !snake.is_empty() {
        variants.insert(snake.clone());
        variants.insert(snake.replace('_', ""));
    }
    variants
}

fn camel_to_snake(token: &str) -> String {
    let mut out = String::new();
    let mut previous_was_lower_or_digit = false;
    for ch in token.chars() {
        if ch.is_ascii_uppercase() {
            if previous_was_lower_or_digit && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = false;
        } else if ch == '-' {
            if !out.ends_with('_') {
                out.push('_');
            }
            previous_was_lower_or_digit = false;
        } else {
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn explicit_query_paths(query: &str) -> impl Iterator<Item = PathBuf> + '_ {
    query
        .split(|ch: char| !is_query_path_char(ch))
        .flat_map(explicit_path_token_variants)
        .filter(|token| token.contains('/') || token.contains('.'))
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"))
        .map(PathBuf::from)
}

fn is_query_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')
}

fn explicit_path_token_variants(token: &str) -> Vec<&str> {
    let token = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'));
    if token.is_empty() {
        return Vec::new();
    }
    let trimmed_sentence_dot = token.trim_end_matches('.');
    if trimmed_sentence_dot != token && !trimmed_sentence_dot.is_empty() {
        vec![token, trimmed_sentence_dot]
    } else {
        vec![token]
    }
}

fn lexical_query_terms(query: &str) -> BTreeSet<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.len() >= 3)
        .map(str::to_lowercase)
        .filter(|term| {
            matches!(
                source_query_term_role(term),
                SourceQueryTermRole::SymbolLike
                    | SourceQueryTermRole::PathLike
                    | SourceQueryTermRole::LexicalHint
            )
        })
        .collect()
}

fn normalize_workspace_relative_path(workspace_root: &Path, path: &Path) -> Option<PathBuf> {
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    let full_path = workspace_root.join(path);
    if !full_path.is_file() {
        return None;
    }
    Some(path.to_path_buf())
}

fn should_skip_repo_context_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".sigil"
                    | ".repo-local-dev"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | "coverage"
                    | ".pytest_cache"
                    | "__pycache__"
            )
        })
}

fn should_skip_repo_context_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".sigil"
                    | ".repo-local-dev"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | "coverage"
                    | ".pytest_cache"
                    | "__pycache__"
            )
        })
    })
}

fn is_secret_like_path(relative: &Path) -> bool {
    let text = relative.to_string_lossy().to_lowercase();
    text.ends_with(".env")
        || text.contains("/.env")
        || text.contains("id_rsa")
        || text.contains("id_ed25519")
        || text.contains("private_key")
        || text.ends_with(".pem")
        || text.ends_with(".key")
}

fn looks_like_text_file(relative: &Path) -> bool {
    relative
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase)
        .is_none_or(|extension| {
            matches!(
                extension.as_str(),
                "rs" | "toml"
                    | "md"
                    | "txt"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "py"
                    | "go"
                    | "java"
                    | "kt"
                    | "swift"
                    | "c"
                    | "h"
                    | "cpp"
                    | "hpp"
                    | "css"
                    | "html"
                    | "sh"
                    | "sql"
            )
        })
}

fn truncate_to_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[..end]
}

fn snippet_window_around_byte(text: &str, byte_index: usize, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let before = max_bytes / 4;
    let mut start = byte_index.saturating_sub(before);
    while !text.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[start..end]
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;
