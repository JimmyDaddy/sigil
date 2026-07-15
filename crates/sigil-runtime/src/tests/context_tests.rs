use std::fs;

use anyhow::{Result, anyhow};
use sigil_code_intel::{CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol, LspContextSnapshot};
use sigil_kernel::{
    ContextBodyRef, ContextInclusionReason, ContextItem, ContextPackOptions,
    ContextScoreComponentKind, ContextSensitivity, ContextSource, ContextTrustLevel, FileChangeRef,
    PluginHookContextOptions, PluginHookOutputEnvelope, PluginHookOutputStream,
    PluginTrustDecision, RedactionState, RuntimeContextCandidates, SourcedFact, TaskMemoryV1,
    pack_context_items,
};

use super::{
    ContextSourcePolicy, ContextSourceProvider, ContextSourceRequest, McpResourceContextItem,
    McpResourceContextProvider, PluginHookContextProvider, RequestContextResolver,
    collect_context_from_source_provider, context_candidates_from_repo_query,
    context_candidates_from_safe_sources, context_items_from_plugin_hook_output,
    context_items_from_task_memory,
};

#[derive(Clone)]
enum TestContextProviderBehavior {
    Candidates(RuntimeContextCandidates),
    Error,
    Panic,
}

struct TestContextProvider {
    source_id: &'static str,
    policy: ContextSourcePolicy,
    behavior: TestContextProviderBehavior,
}

impl ContextSourceProvider for TestContextProvider {
    fn source_id(&self) -> &'static str {
        self.source_id
    }

    fn source_kind(&self) -> ContextSource {
        ContextSource::RepositoryFile
    }

    fn policy(&self) -> ContextSourcePolicy {
        self.policy.clone()
    }

    fn collect(&self, _request: &ContextSourceRequest<'_>) -> Result<RuntimeContextCandidates> {
        match &self.behavior {
            TestContextProviderBehavior::Candidates(candidates) => Ok(candidates.clone()),
            TestContextProviderBehavior::Error => Err(anyhow!("repo context source unavailable")),
            TestContextProviderBehavior::Panic => panic!("repo context source panicked"),
        }
    }
}

fn test_context_source_policy() -> ContextSourcePolicy {
    ContextSourcePolicy {
        max_items: 2,
        max_item_tokens: 3,
        max_total_tokens: 4,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        default_sensitivity: ContextSensitivity::Repository,
        requires_egress_decision: false,
        timeout_ms: 100,
        allow_blocking_io: false,
    }
}

fn test_context_item(id: &str, body: &str, token_cost: usize) -> ContextItem {
    ContextItem {
        id: id.to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: None,
        token_cost,
        score: Some(10.0),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline(body),
    }
}

fn test_runtime_context(items: Vec<(&str, &str, usize)>) -> RuntimeContextCandidates {
    let mut context = RuntimeContextCandidates::new();
    for (id, body, token_cost) in items {
        context.items.push(test_context_item(id, body, token_cost));
        context.snippets.insert(id.to_owned(), body.to_owned());
    }
    context
}

fn has_score_component(item: &ContextItem, kind: ContextScoreComponentKind) -> bool {
    item.score_breakdown
        .iter()
        .any(|component| component.kind == kind && component.value > 0.0)
}

fn code_range() -> CodeRange {
    CodeRange {
        start_line: 12,
        start_character: 4,
        end_line: 12,
        end_character: 24,
    }
}

fn runtime_task_memory() -> TaskMemoryV1 {
    TaskMemoryV1 {
        memory_id: "runtime-memory".to_owned(),
        branch_id: None,
        valid_for_snapshot: "snapshot-runtime".to_owned(),
        supersedes: None,
        source_event_ids: vec!["event-objective".to_owned()],
        objective: "Keep context provenance inspectable".to_owned(),
        active_plan: None,
        constraints: Vec::new(),
        decisions: Vec::new(),
        files_changed: vec![FileChangeRef {
            path: "dev/docs/rfcs/0010-structured-compaction-and-task-memory.md".into(),
            source_event_id: Some("event-file".to_owned()),
            mutation_receipt_id: Some("op-doc".to_owned()),
        }],
        commands_run: Vec::new(),
        verification_results: Vec::new(),
        failed_attempts: Vec::new(),
        risks: Vec::new(),
        unresolved_issues: vec![SourcedFact::system_derived(
            "Context hook runtime remains a later extension slice",
            "event-unresolved",
        )],
    }
}

fn plugin_hook_output(content: &str) -> PluginHookOutputEnvelope {
    PluginHookOutputEnvelope {
        execution_id: "hook-exec-runtime".to_owned(),
        plugin_id: "repo-review".to_owned(),
        hook_id: "context-rules".to_owned(),
        stdout: PluginHookOutputStream {
            content: content.to_owned(),
            total_bytes: content.len() as u64,
            returned_bytes: content.len() as u64,
            omitted_bytes: 0,
            total_lines: 1,
            returned_lines: 1,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        stderr: PluginHookOutputStream {
            content: String::new(),
            total_bytes: 0,
            returned_bytes: 0,
            omitted_bytes: 0,
            total_lines: 0,
            returned_lines: 0,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        artifact_refs: Vec::new(),
        artifact_refs_truncated: false,
        redaction_state: RedactionState::None,
        parse_error: None,
        model_visible_summary: "plugin hook context-rules finished succeeded".to_owned(),
    }
}

#[test]
fn context_retrieves_task_memory_items_with_provenance() -> Result<()> {
    let items = context_items_from_task_memory(&runtime_task_memory())?;

    assert!(items.iter().any(|item| {
        item.id == "task-memory:runtime-memory:objective"
            && item.source == sigil_kernel::ContextSource::TaskDigest
            && item.source_event_id.as_deref() == Some("event-objective")
    }));
    assert!(items.iter().any(|item| {
        item.id == "task-memory:runtime-memory:unresolved:0"
            && item.source_event_id.as_deref() == Some("event-unresolved")
    }));
    assert!(items.iter().any(|item| {
        item.id == "task-memory:runtime-memory:file:0"
            && item.source_event_id.as_deref() == Some("event-file")
    }));
    Ok(())
}

#[test]
fn context_retrieves_plugin_hook_output_with_extension_labels() -> Result<()> {
    let output = plugin_hook_output("Prefer the existing context V0 adapter.");

    let context = context_items_from_plugin_hook_output(
        &output,
        PluginHookContextOptions::new("event-hook"),
    )?;

    assert_eq!(context.items.len(), 1);
    assert_eq!(
        context.items[0].source,
        sigil_kernel::ContextSource::ExtensionProvided
    );
    assert_eq!(
        context.items[0].source_event_id.as_deref(),
        Some("event-hook")
    );
    assert_eq!(
        context
            .snippets
            .get("plugin-hook:repo-review:context-rules:hook-exec-runtime:stdout")
            .map(String::as_str),
        Some("Prefer the existing context V0 adapter.")
    );
    Ok(())
}

#[test]
fn trusted_plugin_context_provider_enters_dynamic_suffix_as_extension() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = PluginHookContextProvider::new(
        plugin_hook_output("Prefer the existing context V0 adapter."),
        PluginHookContextOptions::new("event-hook"),
        PluginTrustDecision::Trusted,
    );

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context rules"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::ExtensionProvided)
        .expect("trusted plugin context row");
    assert_eq!(item.trust_level, ContextTrustLevel::ExtensionProvided);
    assert_eq!(item.inclusion_reason, ContextInclusionReason::RetrievalHit);
    assert!(context.snippets.contains_key(&item.id));

    let packed = pack_context_items(context.items, ContextPackOptions::new(64))?;
    assert!(packed.dynamic_suffix.iter().any(|item| {
        item.source == ContextSource::ExtensionProvided
            && item.inclusion_reason == ContextInclusionReason::RetrievalHit
    }));
    Ok(())
}

#[test]
fn untrusted_plugin_context_provider_excludes_snippet_with_trust_reason() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = PluginHookContextProvider::new(
        plugin_hook_output("Do not show this untrusted extension output."),
        PluginHookContextOptions::new("event-hook"),
        PluginTrustDecision::NeedsReview,
    );

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context rules"),
    )?;

    assert_eq!(context.items.len(), 1);
    assert_eq!(
        context.items[0].inclusion_reason,
        ContextInclusionReason::ExcludedUntrustedWorkspace
    );
    assert_eq!(context.items[0].source, ContextSource::ExtensionProvided);
    assert!(context.snippets.is_empty());
    Ok(())
}

#[test]
fn mcp_resource_context_provider_requires_egress_decision_for_snippet() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = McpResourceContextProvider::new(vec![McpResourceContextItem::new(
        "docs",
        "https://example.test/resource.json",
        Some("application/json".to_owned()),
        r#"{"summary":"external docs"}"#,
    )]);

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "external docs"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::McpResource)
        .expect("mcp resource context row");
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedEgressDenied
    );
    assert_eq!(item.sensitivity, ContextSensitivity::External);
    assert!(!context.snippets.contains_key(&item.id));
    Ok(())
}

#[test]
fn mcp_resource_context_provider_allows_bounded_text_after_egress_decision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = McpResourceContextProvider::new(vec![
        McpResourceContextItem::new(
            "docs",
            "mcp://docs/guide",
            Some("text/markdown; charset=utf-8".to_owned()),
            "# Guide\nUse bounded MCP context.",
        )
        .with_egress_decision("egress-allow-docs"),
    ]);

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "guide"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::McpResource)
        .expect("mcp resource context row");
    assert_eq!(item.inclusion_reason, ContextInclusionReason::RetrievalHit);
    assert_eq!(item.egress_decision.as_deref(), Some("egress-allow-docs"));
    assert_eq!(
        context.snippets.get(&item.id).map(String::as_str),
        Some("# Guide\nUse bounded MCP context.")
    );
    Ok(())
}

#[test]
fn mcp_resource_context_provider_filters_unsupported_media_type() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = McpResourceContextProvider::new(vec![
        McpResourceContextItem::new(
            "images",
            "mcp://images/logo",
            Some("image/png".to_owned()),
            "raw png bytes",
        )
        .with_egress_decision("egress-allow-images"),
    ]);

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "logo"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::McpResource)
        .expect("mcp resource context row");
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(context.snippets.is_empty());
    Ok(())
}

#[test]
fn context_source_provider_contract_rejects_missing_hard_caps() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut policy = test_context_source_policy();
    policy.max_items = 0;
    let provider = TestContextProvider {
        source_id: "repo-provider",
        policy,
        behavior: TestContextProviderBehavior::Candidates(RuntimeContextCandidates::new()),
    };

    let error = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )
    .expect_err("provider without max_items must not attach to the default scheduler");

    assert!(error.to_string().contains("must declare max_items"));
    Ok(())
}

#[test]
fn context_source_provider_contract_excludes_oversized_items_after_collect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = TestContextProvider {
        source_id: "repo-provider",
        policy: test_context_source_policy(),
        behavior: TestContextProviderBehavior::Candidates(test_runtime_context(vec![
            ("repo-provider:small", "alpha beta", 2),
            ("repo-provider:large", "one two three four", 4),
        ])),
    };

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )?;

    let small = context
        .items
        .iter()
        .find(|item| item.id == "repo-provider:small")
        .expect("small item should be present");
    assert_eq!(small.inclusion_reason, ContextInclusionReason::RetrievalHit);
    assert_eq!(
        context
            .snippets
            .get("repo-provider:small")
            .map(String::as_str),
        Some("alpha beta")
    );

    let large = context
        .items
        .iter()
        .find(|item| item.id == "repo-provider:large")
        .expect("large item should be retained as excluded provenance");
    assert_eq!(
        large.inclusion_reason,
        ContextInclusionReason::ExcludedTokenBudget
    );
    assert!(!context.snippets.contains_key("repo-provider:large"));
    Ok(())
}

#[test]
fn context_source_provider_contract_excludes_snippet_that_violates_declared_cost() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = TestContextProvider {
        source_id: "repo-provider",
        policy: test_context_source_policy(),
        behavior: TestContextProviderBehavior::Candidates(test_runtime_context(vec![(
            "repo-provider:mismatch",
            "one two",
            1,
        )])),
    };

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.id == "repo-provider:mismatch")
        .expect("mismatched item should be retained as excluded provenance");
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(!context.snippets.contains_key("repo-provider:mismatch"));
    Ok(())
}

#[test]
fn context_source_provider_contract_enforces_required_egress_decision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut policy = test_context_source_policy();
    policy.default_sensitivity = ContextSensitivity::External;
    policy.requires_egress_decision = true;
    let provider = TestContextProvider {
        source_id: "remote-provider",
        policy,
        behavior: TestContextProviderBehavior::Candidates(test_runtime_context(vec![(
            "remote-provider:item",
            "external context",
            2,
        )])),
    };

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )?;

    let item = context
        .items
        .iter()
        .find(|item| item.id == "remote-provider:item")
        .expect("external item should be retained as excluded provenance");
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedEgressDenied
    );
    assert_eq!(item.sensitivity, ContextSensitivity::External);
    assert!(!context.snippets.contains_key("remote-provider:item"));
    Ok(())
}

#[test]
fn context_source_provider_contract_reports_provider_errors_as_excluded_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = TestContextProvider {
        source_id: "repo-provider",
        policy: test_context_source_policy(),
        behavior: TestContextProviderBehavior::Error,
    };

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )?;

    assert_eq!(context.items.len(), 1);
    assert_eq!(context.items[0].id, "context-source:repo-provider:failure");
    assert_eq!(
        context.items[0].inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(context.snippets.is_empty());
    Ok(())
}

#[test]
fn context_source_provider_contract_catches_provider_panics() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = TestContextProvider {
        source_id: "repo-provider",
        policy: test_context_source_policy(),
        behavior: TestContextProviderBehavior::Panic,
    };

    let context = collect_context_from_source_provider(
        &provider,
        &ContextSourceRequest::new(temp.path(), "context"),
    )?;

    assert_eq!(
        context.items[0].inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(context.snippets.is_empty());
    Ok(())
}

#[test]
fn safe_context_sources_record_lsp_unavailable_without_blocking_repo_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("README.md"), "Sigil context overview")?;

    let context = context_candidates_from_safe_sources(temp.path(), "summarize README.md", None)?;

    assert!(context.items.iter().any(|item| {
        item.id == "repo-file:README.md" && item.source == ContextSource::RepositoryFile
    }));
    let unavailable = context
        .items
        .iter()
        .find(|item| item.id == "lsp-context:unavailable")
        .expect("missing warm LSP unavailable provenance");
    assert_eq!(unavailable.source, ContextSource::LspSymbol);
    assert_eq!(
        unavailable.inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(!context.snippets.contains_key("lsp-context:unavailable"));
    Ok(())
}

#[test]
fn safe_context_sources_include_query_relevant_warm_lsp_rows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("src"))?;
    fs::write(
        temp.path().join("src/context.rs"),
        "pub fn parse_config() {}\n",
    )?;
    let snapshot = LspContextSnapshot::ready()
        .with_symbols(vec![CodeSymbol {
            name: "parse_config".to_owned(),
            kind: "function".to_owned(),
            path: "src/context.rs".to_owned(),
            range: code_range(),
            container_name: Some("context".to_owned()),
        }])
        .with_diagnostics(vec![CodeDiagnostic {
            path: "src/context.rs".to_owned(),
            range: code_range(),
            severity: "warning".to_owned(),
            message: "parse_config has an unused result".to_owned(),
            source: Some("rust-analyzer".to_owned()),
        }])
        .with_references(vec![CodeLocation {
            path: "src/main.rs".to_owned(),
            range: code_range(),
            preview: Some("parse_config();".to_owned()),
        }]);

    let context = context_candidates_from_safe_sources(
        temp.path(),
        "where is `parse_config` used and are there diagnostics?",
        Some(&snapshot),
    )?;

    let symbol = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::LspSymbol)
        .expect("warm symbol context should be included");
    assert_eq!(
        symbol.inclusion_reason,
        ContextInclusionReason::WarmLspMatch
    );
    assert!(symbol.score.is_some());
    assert!(has_score_component(
        symbol,
        ContextScoreComponentKind::ExactSymbol
    ));
    assert!(context.snippets.contains_key(&symbol.id));

    let diagnostic = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::LspDiagnostic)
        .expect("warm diagnostic context should be included");
    assert_eq!(
        diagnostic.inclusion_reason,
        ContextInclusionReason::WarmLspMatch
    );
    assert!(diagnostic.score.is_some());

    let reference = context
        .items
        .iter()
        .find(|item| item.source == ContextSource::LspReference)
        .expect("warm reference context should be included");
    assert_eq!(
        reference.inclusion_reason,
        ContextInclusionReason::WarmLspMatch
    );
    assert!(reference.score.is_some());
    assert!(
        context
            .items
            .iter()
            .all(|item| item.source != ContextSource::RepositoryFile)
    );
    Ok(())
}

#[test]
fn safe_context_sources_fall_back_when_warm_lsp_rows_are_unrelated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("src"))?;
    fs::write(
        temp.path().join("src/config.rs"),
        "pub fn parse_config() {}\n",
    )?;
    let snapshot = LspContextSnapshot::ready().with_symbols(vec![CodeSymbol {
        name: "render_dashboard".to_owned(),
        kind: "function".to_owned(),
        path: "src/dashboard.rs".to_owned(),
        range: code_range(),
        container_name: None,
    }]);

    let context = context_candidates_from_safe_sources(
        temp.path(),
        "where is `parse_config` defined?",
        Some(&snapshot),
    )?;

    assert!(
        context
            .items
            .iter()
            .any(|item| item.id == "repo-file:src/config.rs")
    );
    assert!(context.items.iter().any(|item| {
        item.id == "lsp-context:miss"
            && item.inclusion_reason == ContextInclusionReason::ExcludedUnsupported
    }));
    Ok(())
}

#[tokio::test]
async fn request_context_resolver_without_service_uses_bounded_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("README.md"), "Sigil request context")?;
    let resolver = RequestContextResolver::request_local(temp.path().to_path_buf());

    let context = resolver.resolve("summarize README.md").await?;

    assert!(!resolver.has_shared_code_intelligence());
    assert!(
        context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    assert!(
        context
            .items
            .iter()
            .any(|item| item.id == "lsp-context:unavailable")
    );
    Ok(())
}

#[test]
fn safe_context_sources_record_lsp_timeout_as_excluded_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let snapshot = LspContextSnapshot::timed_out(75);

    let context =
        context_candidates_from_safe_sources(temp.path(), "find `parse_config`", Some(&snapshot))?;

    let timeout = context
        .items
        .iter()
        .find(|item| item.id == "lsp-context:timeout")
        .expect("missing warm LSP timeout provenance");
    assert_eq!(timeout.source, ContextSource::LspSymbol);
    assert_eq!(
        timeout.inclusion_reason,
        ContextInclusionReason::ExcludedUnsupported
    );
    assert!(!context.snippets.contains_key("lsp-context:timeout"));
    Ok(())
}

#[test]
fn context_retrieves_repo_file_candidates_from_query() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join("README.md"),
        "Sigil runtime context provider wiring notes\n",
    )?;

    let context = context_candidates_from_repo_query(temp.path(), "summarize README.md")?;

    assert!(context.items.iter().any(|item| {
        item.id == "repo-file:README.md"
            && item.source == ContextSource::RepositoryFile
            && item.inclusion_reason == ContextInclusionReason::RetrievalHit
    }));
    assert_eq!(
        context
            .snippets
            .get("repo-file:README.md")
            .map(String::as_str),
        Some("Sigil runtime context provider wiring notes\n")
    );
    Ok(())
}

#[test]
fn context_repo_candidates_keep_explicit_path_prompts_precise() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("guides"))?;
    fs::write(
        temp.path().join("guides/setup.md"),
        "English setup guide for the sample workspace.\n",
    )?;
    fs::write(
        temp.path().join("guides/setup.zh-CN.md"),
        "中文安装指南：先配置凭据，再运行检查。\n",
    )?;
    fs::create_dir_all(temp.path().join("packages/installer"))?;
    fs::write(
        temp.path().join("packages/installer/setup.md"),
        "Package installer setup notes for a different component.\n",
    )?;

    let context =
        context_candidates_from_repo_query(temp.path(), "总结：guides/setup.zh-CN.md 的流程")?;

    let ids = context
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["repo-file:guides/setup.zh-CN.md"]);
    assert_eq!(
        context
            .snippets
            .get("repo-file:guides/setup.zh-CN.md")
            .map(String::as_str),
        Some("中文安装指南：先配置凭据，再运行检查。\n")
    );
    Ok(())
}

#[test]
fn context_repo_candidates_keep_lexical_fallback_without_explicit_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("docs/specs"))?;
    fs::write(
        temp.path().join("docs/specs/evaluation-harness.md"),
        "Evaluation harness runner and deterministic model evaluation policy.\n",
    )?;
    fs::write(
        temp.path().join("docs/specs/context-engine.md"),
        "Context engine and retrieval design.\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "which document covers evaluation harness policy",
    )?;

    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:docs/specs/evaluation-harness.md")
    );
    let lexical = context.items.first().expect("lexical fallback candidate");
    assert!(has_score_component(
        lexical,
        ContextScoreComponentKind::RetrievalScore
    ));
    Ok(())
}

#[test]
fn context_repo_candidates_do_not_read_secret_like_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(".env"), "SIGIL_API_KEY=secret-value\n")?;

    let context = context_candidates_from_repo_query(temp.path(), "inspect .env")?;

    let item = context
        .items
        .iter()
        .find(|item| item.id == "repo-file:.env")
        .expect("secret-like file context item");
    assert_eq!(item.source, ContextSource::RepositoryFile);
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    assert_eq!(
        context.snippets.get("repo-file:.env").map(String::as_str),
        Some("secret-like repository file omitted from automatic context")
    );
    assert!(
        !context
            .snippets
            .values()
            .any(|snippet| snippet.contains("secret-value"))
    );
    Ok(())
}

#[test]
fn context_repo_candidates_do_not_read_workspace_sigil_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join("sigil.toml"),
        "[providers.deepseek]\napi_key = \"workspace-config-secret\"\n",
    )?;

    let context = context_candidates_from_repo_query(temp.path(), "inspect sigil.toml")?;

    let item = context
        .items
        .iter()
        .find(|item| item.id == "repo-file:sigil.toml")
        .expect("workspace Sigil config context item");
    assert_eq!(item.source, ContextSource::RepositoryFile);
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    assert_eq!(
        context
            .snippets
            .get("repo-file:sigil.toml")
            .map(String::as_str),
        Some("secret-like repository file omitted from automatic context")
    );
    assert!(
        !context
            .snippets
            .values()
            .any(|snippet| snippet.contains("workspace-config-secret"))
    );
    Ok(())
}

#[test]
fn context_repo_candidates_respect_ignore_rules_in_lexical_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(".gitignore"), "ignored-context.md\n")?;
    fs::write(
        temp.path().join("ignored-context.md"),
        "lexical-only ignored marker must stay out of request context\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "where is the lexical-only ignored marker documented",
    )?;

    assert!(
        context
            .items
            .iter()
            .all(|item| item.id != "repo-file:ignored-context.md")
    );
    assert!(
        context
            .snippets
            .values()
            .all(|snippet| !snippet.contains("lexical-only ignored marker"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn context_repo_candidates_reject_explicit_symlink_paths() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let outside = tempfile::NamedTempFile::new()?;
    fs::write(
        outside.path(),
        "external symlink secret must remain outside\n",
    )?;
    symlink(outside.path(), temp.path().join("linked-context.md"))?;

    let context = context_candidates_from_repo_query(temp.path(), "inspect linked-context.md")?;

    assert!(
        context
            .items
            .iter()
            .all(|item| item.id != "repo-file:linked-context.md")
    );
    assert!(
        context
            .snippets
            .values()
            .all(|snippet| !snippet.contains("external symlink secret"))
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_find_exact_rust_symbol() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-kernel/src"))?;
    let filler = "pub fn unrelated_helper() {}\n".repeat(600);
    fs::write(
        temp.path().join("crates/sigil-kernel/src/session.rs"),
        format!("{filler}pub fn build_request_with_transient_messages_and_context() {{}}\n"),
    )?;
    fs::create_dir_all(temp.path().join("dev/docs"))?;
    fs::write(
        temp.path().join("dev/docs/context.md"),
        "build request context notes without implementation source\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Where is build_request_with_transient_messages_and_context defined in Rust source?",
    )?;

    let source = context
        .items
        .iter()
        .find(|item| item.id == "repo-file:crates/sigil-kernel/src/session.rs")
        .expect("session.rs source candidate");
    assert_eq!(
        source.inclusion_reason,
        ContextInclusionReason::ExactSymbolMatch
    );
    assert!(has_score_component(
        source,
        ContextScoreComponentKind::ExactSymbol
    ));
    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-kernel/src/session.rs")
    );
    assert!(
        context
            .snippets
            .get("repo-file:crates/sigil-kernel/src/session.rs")
            .is_some_and(
                |snippet| snippet.contains("build_request_with_transient_messages_and_context")
            )
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_find_python_and_typescript_symbols() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("services"))?;
    fs::write(
        temp.path().join("services/config.py"),
        "def parse_configuration():\n    return {}\n",
    )?;
    fs::create_dir_all(temp.path().join("web"))?;
    fs::write(
        temp.path().join("web/session.ts"),
        "export function buildSession(): void {}\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Where are parse_configuration and buildSession defined in source?",
    )?;

    for path in ["services/config.py", "web/session.ts"] {
        let item = context
            .items
            .iter()
            .find(|item| item.id == format!("repo-file:{path}"))
            .unwrap_or_else(|| panic!("missing multilingual source candidate {path}"));
        assert_eq!(
            item.inclusion_reason,
            ContextInclusionReason::ExactSymbolMatch
        );
        assert!(has_score_component(
            item,
            ContextScoreComponentKind::ExactSymbol
        ));
    }
    Ok(())
}

#[test]
fn context_source_symbol_candidates_match_cjk_query_and_identifier() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("services"))?;
    fs::write(
        temp.path().join("services/config.py"),
        "def 配置解析器():\n    return {}\n",
    )?;

    let explicit =
        context_candidates_from_repo_query(temp.path(), "请定位 `配置解析器` 函数的源码定义")?;
    let item = explicit
        .items
        .iter()
        .find(|item| item.id == "repo-file:services/config.py")
        .expect("CJK identifier source candidate");
    assert_eq!(
        item.inclusion_reason,
        ContextInclusionReason::ExactSymbolMatch
    );

    let natural = context_candidates_from_repo_query(temp.path(), "配置解析器函数在哪里定义？")?;
    assert_eq!(
        natural.items.first().map(|item| item.id.as_str()),
        Some("repo-file:services/config.py")
    );
    Ok(())
}

#[test]
fn context_source_symbol_snippet_centers_definition_range() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("services"))?;
    let source = format!(
        "# EARLY_DECOY build_session\n{}def build_session():\n    return 'LATE_DEFINITION'\n",
        "# unrelated filler line with enough width\n".repeat(180)
    );
    fs::write(temp.path().join("services/session.py"), source)?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Where is `build_session` defined in source?",
    )?;
    let snippet = context
        .snippets
        .get("repo-file:services/session.py")
        .expect("range-centered source snippet");
    assert!(snippet.contains("LATE_DEFINITION"));
    assert!(!snippet.contains("EARLY_DECOY"));
    Ok(())
}

#[test]
fn context_source_symbol_candidates_rank_source_paths_for_source_intent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "runtime repo file context provider implementation\n",
    )?;
    fs::create_dir_all(temp.path().join("dev/docs/rfcs"))?;
    fs::write(
        temp.path().join("dev/docs/rfcs/context-engine.md"),
        "runtime repo file context provider design notes\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Which Rust source file implements the bounded runtime repo-file context provider?",
    )?;

    let ids = context
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"repo-file:crates/sigil-runtime/src/context.rs"));
    assert_eq!(
        context
            .items
            .iter()
            .find(|item| item.id == "repo-file:crates/sigil-runtime/src/context.rs")
            .map(|item| &item.inclusion_reason),
        Some(&ContextInclusionReason::SourcePathMatch)
    );
    let source_path = context
        .items
        .iter()
        .find(|item| item.id == "repo-file:crates/sigil-runtime/src/context.rs")
        .expect("source path candidate");
    assert!(has_score_component(
        source_path,
        ContextScoreComponentKind::SourcePath
    ));
    Ok(())
}

#[test]
fn context_source_symbol_candidates_do_not_treat_rust_as_symbol() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "runtime context provider implementation\n",
    )?;
    fs::create_dir_all(temp.path().join("crates/sigil-tui/src/app"))?;
    fs::write(
        temp.path()
            .join("crates/sigil-tui/src/app/workspace_trust_flow.rs"),
        "workspace trust gate implementation\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Which Rust source file implements runtime context provider?",
    )?;

    assert!(context.items.iter().all(|item| {
        item.inclusion_reason != ContextInclusionReason::ExactSymbolMatch
            || item.id != "repo-file:crates/sigil-tui/src/app/workspace_trust_flow.rs"
    }));
    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-runtime/src/context.rs")
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_do_not_score_natural_language_terms() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "runtime context provider implementation\n",
    )?;
    fs::create_dir_all(temp.path().join("crates/sigil-noise/src"))?;
    fs::write(
        temp.path().join("crates/sigil-noise/src/noisy.rs"),
        "which where automatic system provided most likely answer output only\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Which Rust source file is most likely provided by automatic system for runtime context provider? Only output the answer.",
    )?;

    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-runtime/src/context.rs")
    );
    assert!(
        context
            .items
            .iter()
            .all(|item| item.id != "repo-file:crates/sigil-noise/src/noisy.rs")
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_preserve_quoted_natural_word_as_symbol() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path().join("crates/sigil-runtime/src/system.rs"),
        "pub fn system() {}\n",
    )?;
    fs::create_dir_all(temp.path().join("crates/sigil-noise/src"))?;
    fs::write(
        temp.path().join("crates/sigil-noise/src/noisy.rs"),
        "which where automatic system provided most likely answer output only\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Where is `system` defined in Rust source?",
    )?;

    let source = context
        .items
        .iter()
        .find(|item| item.id == "repo-file:crates/sigil-runtime/src/system.rs")
        .expect("system.rs source candidate");
    assert_eq!(
        source.inclusion_reason,
        ContextInclusionReason::ExactSymbolMatch
    );
    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-runtime/src/system.rs")
    );
    assert!(
        context
            .snippets
            .get("repo-file:crates/sigil-runtime/src/system.rs")
            .is_some_and(|snippet| snippet.contains("pub fn system"))
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_preserve_code_like_term_after_noise_filter() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path()
            .join("crates/sigil-runtime/src/system_config.rs"),
        "pub struct SystemConfig;\n",
    )?;
    fs::create_dir_all(temp.path().join("crates/sigil-noise/src"))?;
    fs::write(
        temp.path().join("crates/sigil-noise/src/noisy.rs"),
        "which where automatic system provided most likely answer output only\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Where is SystemConfig defined in Rust source?",
    )?;

    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-runtime/src/system_config.rs")
    );
    assert_eq!(
        context.items.first().map(|item| &item.inclusion_reason),
        Some(&ContextInclusionReason::ExactSymbolMatch)
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_prefer_exact_file_stem_symbol_match() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-kernel/src"))?;
    fs::write(
        temp.path()
            .join("crates/sigil-kernel/src/execution_backend.rs"),
        "pub trait ExecutionBackend {}\n",
    )?;
    fs::create_dir_all(
        temp.path()
            .join("crates/sigil-tools-builtin/src/execution_backends"),
    )?;
    fs::write(
        temp.path()
            .join("crates/sigil-tools-builtin/src/execution_backends/mod.rs"),
        "use sigil_kernel::ExecutionBackend;\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "ExecutionBackend trait is defined in which Rust source file?",
    )?;

    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-kernel/src/execution_backend.rs")
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_match_hyphenated_surface_text() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-tui/src/ui"))?;
    fs::write(
        temp.path().join("crates/sigil-tui/src/ui/live_panel.rs"),
        "let title = \"Plan ready\";\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Which Rust source file renders the plan-ready TUI surface?",
    )?;

    assert_eq!(
        context.items.first().map(|item| item.id.as_str()),
        Some("repo-file:crates/sigil-tui/src/ui/live_panel.rs")
    );
    Ok(())
}

#[test]
fn context_source_symbol_candidates_preserve_explicit_path_precision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("crates/sigil-runtime/src"))?;
    fs::write(
        temp.path().join("crates/sigil-runtime/src/context.rs"),
        "pub struct RuntimeContextCandidates;\n",
    )?;
    fs::write(
        temp.path().join("README.md"),
        "RuntimeContextCandidates user documentation\n",
    )?;

    let context = context_candidates_from_repo_query(
        temp.path(),
        "Summarize README.md and mention RuntimeContextCandidates",
    )?;

    let ids = context
        .items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["repo-file:README.md"]);
    assert!(has_score_component(
        &context.items[0],
        ContextScoreComponentKind::ExplicitPath
    ));
    Ok(())
}
