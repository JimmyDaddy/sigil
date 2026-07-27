use std::{
    collections::VecDeque,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use clap::{CommandFactory, Parser};
use futures::{Stream, stream};
use sigil_kernel::{
    EventHandler, JsonlSessionStore, ModelMessage, ProviderChunk, PublicRunEventKind,
    PublicTaskPhase, RootConfig, RunEvent, SessionConfig, StorageConfig, ToolAccess, ToolCall,
    ToolCategory, ToolErrorKind, ToolExecutionId, ToolPreview, ToolPreviewCapability,
    ToolProgressEvent, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, UsageStats,
    WorkspaceTrust, resolve_workspace_root, workspace_trust_from_entries,
};
use sigil_runtime::application_run::{application_run_input, default_application_session_path};
use sigil_runtime::doctor::{DoctorCheck, DoctorReport, DoctorStatus};
use sigil_runtime::machine_protocol::MachineExitCode;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::intent_cli::IntentCommand;
use super::{
    BuildInfo, Cli, Commands, DEFAULT_HTTP_TOKEN_ENV, DoctorOutput, HTTP_SERVER_STATE_DIR,
    RunOutput, ServeOptions, ServeOwnerChannelWatcher, ServeStartupOutput, ServeStartupPlan,
    StdoutEventHandler, build_serve_startup_plan, build_session_catalog_service,
    cli_application_run_request, drain_provider_stream, load_serve_root_config,
    render_cli_doctor_report, render_doctor_report, render_provider_chunk, render_run_event,
    render_serve_startup_json, render_serve_startup_plan, render_version,
    run_machine_command_with_cancellation, run_machine_command_with_writer,
};

fn boxed_chunk_stream(
    chunks: Vec<Result<ProviderChunk>>,
) -> Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> {
    Box::pin(stream::iter(chunks))
}

#[test]
fn resolve_workspace_root_uses_config_parent() -> Result<()> {
    let config_path = std::env::temp_dir()
        .join("sigil-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-launch");
    let resolved = resolve_workspace_root(&config_path, &launch_cwd, "workspace/project");

    assert_eq!(
        resolved,
        config_path
            .parent()
            .expect("config path should have a parent")
            .join("workspace/project")
    );
    Ok(())
}

#[test]
fn serve_session_catalog_service_uses_resolved_global_projection_path() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let paths = sigil_runtime::resolve_sigil_paths(
        &StorageConfig::default(),
        &SessionConfig::default(),
        workspace.path(),
    );

    let service = build_session_catalog_service(&paths);

    assert_eq!(service.database_path(), paths.session_catalog_db);
    assert_eq!(HTTP_SERVER_STATE_DIR, "http-server-v2");
    assert_eq!(
        paths.session_catalog_db.parent(),
        Some(paths.projections_root.as_path())
    );
    Ok(())
}

#[test]
fn serve_root_config_uses_setup_shell_only_for_an_absent_config() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("missing-sigil.toml");

    let config = load_serve_root_config(&config_path)?;

    assert_eq!(config.workspace.root, ".");
    assert!(config.agent.provider.is_empty());
    assert!(config.agent.connection.is_none());
    assert!(config.agent.model.is_empty());
    assert!(config.providers.is_empty());
    assert!(config.connections.is_empty());
    assert!(!config_path.exists());
    Ok(())
}

#[test]
fn serve_root_config_loads_valid_config_and_rejects_malformed_existing_config() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    fs::write(
        &config_path,
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;

    let config = load_serve_root_config(&config_path)?;
    assert_eq!(config.agent.provider, "deepseek");
    assert_eq!(config.agent.model, "deepseek-v4-flash");

    fs::write(&config_path, "[agent\nprovider = \"deepseek\"")?;
    assert!(load_serve_root_config(&config_path).is_err());
    Ok(())
}

#[test]
fn cli_parses_hidden_model_eval_command_options() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "--config",
        "/tmp/sigil.toml",
        "model-eval",
        "--case",
        "small-code-edit",
        "--repetitions",
        "3",
        "--max-cost-usd",
        "0.50",
        "--timeout-secs",
        "120",
        "--output-dir",
        "/tmp/model-eval",
        "--orchestration-route-contract",
        "/tmp/orchestration-route.toml",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::ModelEval {
            cases,
            repetitions: 3,
            max_cost_usd,
            timeout_secs: 120,
            output_dir,
            orchestration_route_contract,
        }) if cases == ["small-code-edit"]
            && max_cost_usd == "0.50"
            && output_dir == Path::new("/tmp/model-eval")
            && orchestration_route_contract
                == Some(PathBuf::from("/tmp/orchestration-route.toml"))
    ));
    Ok(())
}

#[test]
fn cli_parses_exact_intent_stack_automation_commands() -> Result<()> {
    let inspect = Cli::try_parse_from(["sigil", "intent", "--session", "session-1", "inspect"])?;
    assert!(matches!(
        inspect.command,
        Some(Commands::Intent {
            session,
            command: IntentCommand::Inspect,
        }) if session == "session-1"
    ));

    let preview = Cli::try_parse_from([
        "sigil",
        "intent",
        "--session",
        "session-1",
        "drop-preview",
        "--intent-id",
        "intent-core",
        "--intent-version",
        "2",
    ])?;
    assert!(matches!(
        preview.command,
        Some(Commands::Intent {
            session,
            command: IntentCommand::DropPreview {
                intent_id,
                intent_version: 2,
            },
        }) if session == "session-1" && intent_id == "intent-core"
    ));

    let drop = Cli::try_parse_from([
        "sigil",
        "intent",
        "--session",
        "session-1",
        "drop",
        "--operation-id",
        "operation-drop-core",
        "--stack-version",
        "4",
        "--preview-digest",
        "sha256:jcs-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ])?;
    assert!(matches!(
        drop.command,
        Some(Commands::Intent {
            session,
            command: IntentCommand::Drop {
                operation_id,
                stack_version: 4,
                preview_digest,
            },
        }) if session == "session-1"
            && operation_id == "operation-drop-core"
            && preview_digest.starts_with("sha256:jcs-v1:")
    ));
    Ok(())
}

#[test]
fn cli_parses_hidden_model_eval_route_contract_command() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "--config",
        "/tmp/sigil.toml",
        "model-eval-route-contract",
        "--case",
        "orchestration-v1",
        "--output",
        "/tmp/route.toml",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::ModelEvalRouteContract { cases, output })
            if cases == ["orchestration-v1"] && output == Path::new("/tmp/route.toml")
    ));
    Ok(())
}

#[test]
fn cli_parses_hidden_model_eval_rollout_manifest_command() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "model-eval-rollout-manifest",
        "--report",
        "/tmp/eval-manifest.json",
        "--output",
        "/tmp/sigil-orchestration-rollout-v1.json",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::ModelEvalRolloutManifest { report, output })
            if report == Path::new("/tmp/eval-manifest.json")
                && output == Path::new("/tmp/sigil-orchestration-rollout-v1.json")
    ));
    Ok(())
}

#[test]
fn model_eval_cost_and_case_preflight_are_fail_closed() -> Result<()> {
    assert_eq!(super::parse_model_eval_cost_microusd("0.50")?, 500_000);
    assert!(super::parse_model_eval_cost_microusd("0").is_err());
    assert!(super::parse_model_eval_cost_microusd("NaN").is_err());
    let root = unique_temp_workspace("sigil-model-eval-cases")?;
    assert!(super::resolve_model_eval_fixture_roots(&root, &["../escape".to_owned()]).is_err());
    assert!(super::resolve_model_eval_fixture_roots(&root, &["/absolute".to_owned()]).is_err());
    assert_eq!(
        super::resolve_model_eval_fixture_roots(
            &root,
            &[
                "small-doc-edit".to_owned(),
                "orchestration/positive/cross-layer".to_owned(),
            ],
        )?,
        [
            root.join("dev/evals/model-fixtures/small-doc-edit"),
            root.join("dev/evals/model-fixtures/orchestration/positive/cross-layer"),
        ]
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn model_eval_frozen_orchestration_selector_expands_exact_corpus() -> Result<()> {
    let root = unique_temp_workspace("sigil-model-eval-orchestration-corpus")?;
    let corpus_root = root.join("dev/evals/model-fixtures/orchestration-v1");
    for (case_class, count) in [("negative", 20), ("positive", 10)] {
        for index in 0..count {
            fs::create_dir_all(
                corpus_root
                    .join(case_class)
                    .join(format!("case-{index:02}")),
            )?;
        }
    }

    let cases = super::resolve_model_eval_fixture_roots(&root, &["orchestration-v1".to_owned()])?;
    assert_eq!(cases.len(), 30);
    assert!(
        cases
            .iter()
            .take(20)
            .all(|path| path.starts_with(corpus_root.join("negative")))
    );
    assert!(
        cases
            .iter()
            .skip(20)
            .all(|path| path.starts_with(corpus_root.join("positive")))
    );

    fs::remove_dir_all(corpus_root.join("positive/case-09"))?;
    let error = super::resolve_model_eval_fixture_roots(&root, &["orchestration-v1".to_owned()])
        .expect_err("an incomplete frozen corpus must fail closed");
    assert!(error.to_string().contains("exactly 30 cases"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn model_eval_orchestration_route_contract_loader_is_bounded_and_typed() -> Result<()> {
    let root = unique_temp_workspace("sigil-model-eval-route-contract")?;
    let path = root.join("route.toml");
    let digest = format!("sha256:{}", "a".repeat(64));
    let contract = sigil_runtime::model_eval::ModelEvalOrchestrationRouteContractV1 {
        schema_version: 1,
        provider_kind: "deepseek".to_owned(),
        endpoint_family: "openai-compatible-chat".to_owned(),
        canonical_model_version: "test-v1".to_owned(),
        routing_prompt_digest: digest.clone(),
        planner_prompt_digest: digest.clone(),
        system_prompt_digest: digest.clone(),
        tool_profile_contract_digest: digest,
        sigil_commit: "test-commit".to_owned(),
        sigil_build: "test-build".to_owned(),
    };
    fs::write(&path, toml::to_string(&contract)?)?;

    assert_eq!(
        super::load_model_eval_orchestration_route_contract(&root, Path::new("route.toml"))?,
        contract
    );

    fs::write(&path, vec![b'x'; 64 * 1024 + 1])?;
    assert!(
        super::load_model_eval_orchestration_route_contract(&root, Path::new("route.toml"))
            .is_err()
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn model_eval_manifest_requires_every_requested_repetition() -> Result<()> {
    let mut manifest = sigil_kernel::ModelEvalReportManifestV3 {
        report_schema_version: 3,
        campaign_id: "campaign-test".to_owned(),
        mode: "model".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 15,
        provider_admitted_repetitions: 15,
        completed_repetitions: 15,
        skipped_repetitions: 0,
        accepted_repetitions: 15,
        charged_microusd: 499_995,
        results_jsonl_path: PathBuf::from("results.jsonl"),
        summary_path: PathBuf::from("summary.md"),
        trend_buckets: Vec::new(),
    };
    super::validate_model_eval_manifest(&manifest)?;

    manifest.provider_admitted_repetitions = 14;
    manifest.completed_repetitions = 14;
    manifest.skipped_repetitions = 1;
    manifest.accepted_repetitions = 14;
    let error = super::validate_model_eval_manifest(&manifest)
        .expect_err("one skipped repetition must fail campaign acceptance");
    assert!(error.to_string().contains("requested 15"));
    assert!(error.to_string().contains("skipped 1"));

    manifest.requested_repetitions = 0;
    manifest.provider_admitted_repetitions = 0;
    manifest.completed_repetitions = 0;
    manifest.skipped_repetitions = 0;
    manifest.accepted_repetitions = 0;
    assert!(super::validate_model_eval_manifest(&manifest).is_err());
    Ok(())
}

#[test]
fn orchestration_eval_manifest_requires_qualified_route_gates() -> Result<()> {
    let mut manifest = orchestration_eval_manifest(
        sigil_kernel::OrchestrationEvalRouteStatus::Qualified,
        Vec::new(),
    );
    super::validate_orchestration_eval_manifest(&manifest)?;

    for status in [
        sigil_kernel::OrchestrationEvalRouteStatus::InsufficientEvidence,
        sigil_kernel::OrchestrationEvalRouteStatus::Blocked,
        sigil_kernel::OrchestrationEvalRouteStatus::Stale,
    ] {
        manifest.route_gates[0].status = status;
        manifest.route_gates[0].reasons = vec!["test rejection".to_owned()];
        let error = super::validate_orchestration_eval_manifest(&manifest)
            .expect_err("a non-qualified route must fail campaign acceptance");
        assert!(error.to_string().contains("sha256:route"));
        assert!(error.to_string().contains("test rejection"));
    }

    manifest.route_gates.clear();
    assert!(super::validate_orchestration_eval_manifest(&manifest).is_err());
    Ok(())
}

fn orchestration_eval_manifest(
    status: sigil_kernel::OrchestrationEvalRouteStatus,
    reasons: Vec<String>,
) -> sigil_kernel::OrchestrationEvalReportManifestV1 {
    sigil_kernel::OrchestrationEvalReportManifestV1 {
        report_schema_version: 1,
        campaign_id: "campaign-test".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 30,
        results_jsonl_path: PathBuf::from("results.jsonl"),
        summary_path: PathBuf::from("summary.md"),
        route_gates: vec![sigil_kernel::OrchestrationEvalRouteGateV1 {
            identity: sigil_kernel::OrchestrationEvalRouteIdentityV1 {
                provider_adapter: "test-adapter".to_owned(),
                provider_kind: "test-provider".to_owned(),
                endpoint_family: "test-endpoint".to_owned(),
                canonical_model_id: "test-model".to_owned(),
                canonical_model_version: "test-version".to_owned(),
                route_fingerprint: "sha256:route".to_owned(),
                routing_prompt_digest: "sha256:routing-prompt".to_owned(),
                planner_prompt_digest: "sha256:planner-prompt".to_owned(),
                system_prompt_digest: "sha256:system-prompt".to_owned(),
                tool_profile_contract_digest: "sha256:tool-profile".to_owned(),
                task_config_digest: "sha256:task-config".to_owned(),
                corpus_version: "rfc-0053-orchestration-v1".to_owned(),
                corpus_digest: "sha256:corpus".to_owned(),
                sigil_commit: "test-commit".to_owned(),
                sigil_build: "test-build".to_owned(),
            },
            identity_digest: "sha256:route".to_owned(),
            status,
            negative_cases: 20,
            positive_cases: 10,
            eligible_negative_cases: 20,
            eligible_positive_cases: 10,
            provider_admitted_repetitions: 30,
            completed_repetitions: 30,
            false_positive_rate_ppm: Some(0),
            positive_miss_rate_ppm: Some(0),
            cases_with_majority_misroute: 0,
            cases_with_duplicate_repetition_identity: 0,
            hard_invariant_violations: 0,
            reasons,
        }],
    }
}

#[test]
fn resolve_workspace_root_uses_launch_cwd_for_default_dot() {
    let config_path = std::env::temp_dir()
        .join("sigil-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-launch");

    let resolved = resolve_workspace_root(&config_path, &launch_cwd, ".");

    assert_eq!(resolved, launch_cwd);
}

#[test]
fn default_session_path_uses_configured_log_dir_and_jsonl_suffix() {
    let workspace_root = std::env::temp_dir().join("sigil-workspace");
    let session_dir = workspace_root.join("state/sessions");
    let session_path = default_application_session_path(&session_dir);

    assert!(session_path.starts_with(session_dir));
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    assert!(
        session_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
    );
}

#[test]
fn fresh_cli_session_projects_unknown_workspace_trust() -> Result<()> {
    let workspace = unique_temp_workspace("sigil-cli-workspace-trust")?;
    let store = JsonlSessionStore::new(workspace.join("session.jsonl"))?;

    let session = sigil_kernel::Session::load_from_store("deepseek", "deepseek-test", store)?;
    let trust = workspace_trust_from_entries(session.entries(), &workspace)?;

    assert_eq!(trust, WorkspaceTrust::Unknown);
    Ok(())
}

#[test]
fn run_input_with_repo_context_attaches_repository_candidates() -> Result<()> {
    let workspace = unique_temp_workspace("sigil-cli-context")?;
    fs::write(
        workspace.join("README.md"),
        "Sigil is a TUI-first Rust coding agent.",
    )?;

    let input = application_run_input(&workspace, "summarize README.md".to_owned());

    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md"
            && matches!(item.source, sigil_kernel::ContextSource::RepositoryFile)
    }));
    fs::remove_dir_all(workspace)?;
    Ok(())
}

fn unique_temp_workspace(prefix: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

#[test]
fn render_provider_chunk_formats_text_reasoning_usage_and_done() {
    let text = render_provider_chunk(ProviderChunk::TextDelta("hello".to_owned()));
    assert_eq!(text.stdout, "hello");
    assert!(!text.stop);

    let reasoning = render_provider_chunk(ProviderChunk::ReasoningSummaryDelta("plan".to_owned()));
    assert_eq!(reasoning.stderr, "[reasoning] plan");

    let usage = render_provider_chunk(ProviderChunk::Usage(UsageStats {
        prompt_tokens: 7,
        completion_tokens: 3,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: Some("fp-1".to_owned()),
    }));
    assert!(
        usage
            .stderr
            .contains("[usage] prompt=7 completion=3 fingerprint=fp-1")
    );

    let done = render_provider_chunk(ProviderChunk::Done);
    assert!(done.stop);
}

#[test]
fn render_run_event_formats_tool_events_usage_and_notice() {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        args_json: "{\"path\":\"src/main.rs\"}".to_owned(),
    };
    let spec = ToolSpec {
        name: "write_file".to_owned(),
        description: "write".to_owned(),
        input_schema: Default::default(),
        category: ToolCategory::File,
        access: ToolAccess::Write,
        network_effect: None,
        preview: ToolPreviewCapability::Required,
    };
    let approval = render_run_event(RunEvent::ToolApprovalRequested {
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("src/main.rs", "src/main.rs")],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(ToolPreview {
            title: "Write".to_owned(),
            summary: "1 file changed".to_owned(),
            body: String::new(),
            changed_files: vec!["src/main.rs".to_owned()],
            file_diffs: Vec::new(),
        }),
    });
    assert!(
        approval
            .stderr
            .contains("[tool:approval] write_file (call-1) file write")
    );
    assert!(approval.stderr.contains("network=none risk=medium"));
    assert!(
        approval
            .stderr
            .contains("policy=local:ask network:allow source:allow final:ask")
    );
    assert!(approval.stderr.contains("[tool:preview] 1 file changed"));

    let args = render_run_event(RunEvent::ToolCallArgsDelta {
        id: "call-1".to_owned(),
        delta: "{\"path\":\"src/main.rs\"}".to_owned(),
    });
    assert!(args.stderr.contains("[tool:args:call-1]"));

    let result = render_run_event(RunEvent::ToolResult(ToolResult::error(
        "call-1",
        "write_file",
        sigil_kernel::ToolErrorKind::PermissionDenied,
        "denied",
    )));
    assert!(
        result
            .stderr
            .contains("[tool:result] write_file error=true denied")
    );

    let progress = render_run_event(RunEvent::ToolProgress(ToolProgressEvent {
        execution_id: ToolExecutionId::new("execution-1").expect("valid tool execution id"),
        call_id: "call-1".to_owned(),
        tool_name: "terminal_start".to_owned(),
        sequence: 1,
        status: "running".to_owned(),
        message: Some("running workspace check".to_owned()),
        output_preview: Some("Compiling sigil".to_owned()),
        output_log_ref: None,
        total_bytes: Some(128),
        updated_at_ms: Some(10),
        details: serde_json::json!({"task_id": "terminal-1"}),
    }));
    assert!(
        progress
            .stderr
            .contains("[tool:progress] terminal_start (call-1) running")
    );
    assert!(
        progress
            .stderr
            .contains("[tool:progress:message] running workspace check")
    );
    assert!(
        progress
            .stderr
            .contains("[tool:progress:preview] Compiling sigil")
    );

    let usage = render_run_event(RunEvent::Usage(UsageStats {
        prompt_tokens: 9,
        completion_tokens: 4,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: Some("fp-2".to_owned()),
    }));
    assert!(
        usage
            .stderr
            .contains("[usage] prompt=9 completion=4 fingerprint=fp-2")
    );

    let notice = render_run_event(RunEvent::Notice("heads up".to_owned()));
    assert_eq!(notice.stderr, "[notice] heads up\n");
}

#[tokio::test]
async fn drain_provider_stream_and_stdout_event_handler_accept_supported_events() -> Result<()> {
    let mut provider_stream: std::pin::Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> =
        Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("hello".to_owned())),
            Ok(ProviderChunk::ReasoningDelta("think".to_owned())),
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 1,
                completion_tokens: 2,
                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: Some("fp".to_owned()),
            })),
            Ok(ProviderChunk::Done),
            Ok(ProviderChunk::TextDelta("ignored after done".to_owned())),
        ]));

    drain_provider_stream(&mut provider_stream).await?;

    let mut handler = StdoutEventHandler;
    handler.handle(RunEvent::TextDelta("hello".to_owned()))?;
    handler.handle(RunEvent::ReasoningDelta("think".to_owned()))?;
    handler.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    handler.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    handler.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: false,
        reason: Some("blocked".to_owned()),
    })?;
    handler.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "read_file",
        "ok",
        ToolResultMeta::default(),
    )))?;
    handler.handle(RunEvent::ContinuationState(
        sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "kind".to_owned(),
            message_id: None,
            opaque_blob: Default::default(),
        },
    ))?;
    handler.handle(RunEvent::AssistantMessage(
        sigil_kernel::ModelMessage::assistant(None, Vec::new()),
    ))?;
    Ok(())
}

#[test]
fn cli_parses_hidden_fim_command_options() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "fim",
        "prefix",
        "--suffix",
        "tail",
        "--stop",
        "<eof>",
        "--model",
        "deepseek-test",
        "--max-tokens",
        "64",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Fim {
            ref prompt,
            ref suffix,
            ref stop,
            ref model,
            max_tokens,
        }) if prompt == "prefix"
            && suffix == "tail"
            && stop == &vec!["<eof>".to_owned()]
            && model.as_deref() == Some("deepseek-test")
            && max_tokens == Some(64)
    ));
    Ok(())
}

#[test]
fn cli_parses_hidden_prefix_command_options() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "prefix",
        "prompt",
        "--assistant-prefix",
        "seed",
        "--stop",
        "\n\n",
        "--model",
        "deepseek-test",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Prefix {
            ref prompt,
            ref assistant_prefix,
            ref stop,
            ref model,
        }) if prompt == "prompt"
            && assistant_prefix == "seed"
            && stop == &vec!["\n\n".to_owned()]
            && model.as_deref() == Some("deepseek-test")
    ));
    Ok(())
}

#[test]
fn cli_help_hides_provider_debug_commands() {
    let help = Cli::command().render_long_help().to_string();

    assert!(help.contains("run"));
    assert!(help.contains("resume"));
    assert!(help.contains("doctor"));
    assert!(help.contains("intent"));
    assert!(help.contains("mcp"));
    assert!(help.contains("serve"));
    assert!(!help.contains("prefix"));
    assert!(!help.contains("fim"));
    assert!(!help.contains("model-eval"));
}

#[test]
fn text_cli_ignores_structured_task_state_events() {
    let events = [
        PublicRunEventKind::TaskRoutingChanged {
            handoff_id: "handoff-1".to_owned(),
            status: "accepted".to_owned(),
            task_id: Some("task-1".to_owned()),
        },
        PublicRunEventKind::TaskPhaseChanged {
            task_id: Some("task-1".to_owned()),
            phase: PublicTaskPhase::Planning,
            status: "running".to_owned(),
        },
        PublicRunEventKind::TaskPlanUpdated {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            status: "accepted".to_owned(),
            steps: Vec::new(),
        },
        PublicRunEventKind::TaskBatchChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            batch_id: "batch-1".to_owned(),
            active: 1,
            completed: 0,
            failed: 0,
        },
        PublicRunEventKind::TaskStepChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            step_id: "step-1".to_owned(),
            attempt_id: Some("attempt-1".to_owned()),
            status: "running".to_owned(),
        },
        PublicRunEventKind::IntegrationLaneChanged {
            task_id: "task-1".to_owned(),
            plan_version: 1,
            plan_id: "plan-1".to_owned(),
            lane_id: "lane-1".to_owned(),
            status: "pending".to_owned(),
            conflicts: Vec::new(),
        },
    ];

    for event in events {
        assert_eq!(
            super::render_public_run_event(event),
            super::RenderedOutput::default()
        );
    }
}

#[test]
fn cli_parses_run_command_with_explicit_config() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "run", "hello"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(
        cli.command,
        Some(Commands::Run {
            ref prompt,
            output: RunOutput::Text,
            ..
        }) if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn cli_requires_and_preserves_compound_model_route() -> Result<()> {
    assert!(
        Cli::try_parse_from(["sigil", "run", "hello", "--connection", "openai-personal"]).is_err()
    );
    assert!(Cli::try_parse_from(["sigil", "run", "hello", "--model", "gpt-4.1"]).is_err());
    let cli = Cli::try_parse_from([
        "sigil",
        "run",
        "hello",
        "--connection",
        "openai-personal",
        "--model",
        "gpt-4.1",
    ])?;
    assert!(matches!(
        cli.command,
        Some(Commands::Run {
            connection: Some(ref connection),
            model: Some(ref model),
            ..
        }) if connection == "openai-personal" && model == "gpt-4.1"
    ));

    let request = cli_application_run_request(
        Path::new("sigil.toml"),
        Path::new("."),
        "hello".to_owned(),
        Some("openai-personal"),
        Some("gpt-4.1"),
    )
    .expect("compound route should become one application request");
    assert_eq!(
        request
            .model_connection_id
            .as_ref()
            .map(sigil_kernel::ConnectionId::as_str),
        Some("openai-personal")
    );
    assert_eq!(request.model_name.as_deref(), Some("gpt-4.1"));
    Ok(())
}

#[test]
fn cli_parses_run_machine_output_modes() -> Result<()> {
    for (label, expected) in [("json", RunOutput::Json), ("jsonl", RunOutput::Jsonl)] {
        let cli = Cli::try_parse_from(["sigil", "run", "hello", "--output", label])?;
        assert!(matches!(
            cli.command,
            Some(Commands::Run { output, .. }) if output == expected
        ));
    }
    assert!(Cli::try_parse_from(["sigil", "run", "hello", "--output", "xml"]).is_err());
    Ok(())
}

#[test]
fn cli_parses_resume_command_with_explicit_config_and_session_id() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "resume", "session-123"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(
        cli.command,
        Some(Commands::Resume { ref session }) if session.as_deref() == Some("session-123")
    ));
    Ok(())
}

#[test]
fn cli_parses_resume_command_without_selector_as_latest() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "resume"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Resume { ref session }) if session.is_none()
    ));
    Ok(())
}

#[test]
fn cli_parses_doctor_command_with_explicit_config() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--config", "custom.toml", "doctor"])?;

    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("custom.toml"))
    );
    assert!(matches!(
        cli.command,
        Some(Commands::Doctor {
            output: DoctorOutput::Text,
        })
    ));
    Ok(())
}

#[test]
fn cli_parses_doctor_json_output() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "doctor", "--output", "json"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Doctor {
            output: DoctorOutput::Json,
        })
    ));
    Ok(())
}

#[test]
fn cli_parses_mcp_management_commands() -> Result<()> {
    let add = Cli::try_parse_from([
        "sigil",
        "--config",
        "custom.toml",
        "mcp",
        "add",
        "filesystem",
        "--inherit-env",
        "MCP_TOKEN",
        "--",
        "node",
        "server.js",
    ])?;
    assert!(matches!(
        add.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Add {
                ref name,
                url: None,
                ref inherit_env,
                ref command,
                ..
            },
        }) if name == "filesystem"
            && inherit_env.iter().map(String::as_str).eq(["MCP_TOKEN"])
            && command.iter().map(String::as_str).eq(["node", "server.js"])
    ));

    let remote = Cli::try_parse_from([
        "sigil",
        "mcp",
        "add",
        "search",
        "--url",
        "https://mcp.example.com/mcp",
        "--bearer-token-env-var",
        "SEARCH_TOKEN",
    ])?;
    assert!(matches!(
        remote.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Add {
                ref name,
                ref url,
                ref bearer_token_env_var,
                ref command,
                ..
            },
        }) if name == "search"
            && url.as_deref() == Some("https://mcp.example.com/mcp")
            && bearer_token_env_var.as_deref() == Some("SEARCH_TOKEN")
            && command.is_empty()
    ));

    let list = Cli::try_parse_from(["sigil", "mcp", "list", "--json"])?;
    assert!(matches!(
        list.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::List { json: true },
        })
    ));
    let remove = Cli::try_parse_from(["sigil", "mcp", "remove", "search"])?;
    assert!(matches!(
        remove.command,
        Some(Commands::Mcp {
            command: super::mcp_cli::McpCommand::Remove { ref name },
        }) if name == "search"
    ));
    Ok(())
}

#[test]
fn cli_parses_explicit_tokenizer_install_command() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "tokenizer", "install", "deepseek-v4-flash"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Tokenizer {
            command: super::TokenizerCommand::Install { ref profile },
        }) if profile == "deepseek-v4-flash"
    ));
    Ok(())
}

#[test]
fn cli_parses_serve_command_with_secure_defaults() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "serve"])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Serve {
            host,
            port: 0,
            ref token_env,
            no_token: false,
            startup_output: ServeStartupOutput::Text,
            shutdown_on_stdin_close: false,
        }) if host == IpAddr::V4(Ipv4Addr::LOCALHOST)
            && token_env == DEFAULT_HTTP_TOKEN_ENV
    ));
    Ok(())
}

#[test]
fn cli_parses_serve_command_overrides() -> Result<()> {
    let cli = Cli::try_parse_from([
        "sigil",
        "serve",
        "--host",
        "0.0.0.0",
        "--port",
        "8765",
        "--token-env",
        "CUSTOM_SIGIL_HTTP_TOKEN",
        "--no-token",
        "--startup-output",
        "json",
        "--shutdown-on-stdin-close",
    ])?;

    assert!(matches!(
        cli.command,
        Some(Commands::Serve {
            host,
            port: 8765,
            ref token_env,
            no_token: true,
            startup_output: ServeStartupOutput::Json,
            shutdown_on_stdin_close: true,
        }) if host == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            && token_env == "CUSTOM_SIGIL_HTTP_TOKEN"
    ));
    Ok(())
}

#[test]
fn cli_parses_version_flag_without_subcommand() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil", "--version"])?;

    assert!(cli.show_version);
    assert!(cli.command.is_none());
    Ok(())
}

#[test]
fn cli_without_subcommand_defaults_to_tui() -> Result<()> {
    let cli = Cli::try_parse_from(["sigil"])?;

    assert!(!cli.show_version);
    assert!(cli.command.is_none());
    Ok(())
}

#[test]
fn render_version_includes_build_metadata() {
    let rendered = render_version(BuildInfo {
        version: "1.2.3",
        git_hash: "abc123def456",
        target: "test-target",
        profile: "release",
    });

    assert!(rendered.contains("sigil 1.2.3"));
    assert!(rendered.contains("commit: abc123def456"));
    assert!(rendered.contains("target: test-target"));
    assert!(rendered.contains("profile: release"));
}

#[test]
fn build_info_current_uses_compile_time_metadata() {
    let info = BuildInfo::current();

    assert!(!info.version.is_empty());
    assert!(!info.git_hash.is_empty());
    assert!(!info.target.is_empty());
    assert!(!info.profile.is_empty());
}

#[test]
fn build_info_projects_exactly_into_tui_support_metadata() {
    let support: sigil_runtime::support::SupportBuildInfo = BuildInfo {
        version: "1.2.3",
        git_hash: "abc123",
        target: "test-target",
        profile: "release",
    }
    .into();

    assert_eq!(support.version, "1.2.3");
    assert_eq!(support.commit, "abc123");
    assert_eq!(support.target, "test-target");
    assert_eq!(support.profile, "release");
}

#[test]
fn render_doctor_report_formats_checks_and_summary() {
    let report = DoctorReport {
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Ok,
                name: "config:load".to_owned(),
                message: "config parsed".to_owned(),
                remediation: None,
            },
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: "terminal".to_owned(),
                message: "TERM is not set".to_owned(),
                remediation: Some("set TERM in the shell before launching the TUI".to_owned()),
            },
        ],
    };

    let rendered = render_doctor_report(&report);

    assert!(rendered.contains("Sigil doctor"));
    assert!(rendered.contains("[ok] config:load - config parsed"));
    assert!(rendered.contains("[warn] terminal - TERM is not set"));
    assert!(rendered.contains("fix: set TERM in the shell before launching the TUI"));
    assert!(rendered.contains("summary: warn"));
}

#[test]
fn serve_startup_plan_requires_token_by_default() {
    let error = build_serve_startup_plan(default_serve_options(), None)
        .expect_err("serve should require token by default");

    assert!(error.to_string().contains(DEFAULT_HTTP_TOKEN_ENV));
}

#[test]
fn serve_startup_plan_rejects_every_external_bind() {
    let options = ServeOptions {
        host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ..default_serve_options()
    };

    let error = build_serve_startup_plan(options, Some("secret-token"))
        .expect_err("V1 external bind should be rejected");

    assert!(
        error
            .to_string()
            .contains("only accepts loopback bind addresses")
    );
}

#[test]
fn serve_startup_plan_renders_listener_status_without_token_value() -> Result<()> {
    let plan = build_serve_startup_plan(default_serve_options(), Some("secret-token"))?;
    let rendered = render_serve_startup_plan(&plan);

    assert_eq!(plan.bind_addr, SocketAddr::from(([127, 0, 0, 1], 0)));
    assert!(plan.token_required);
    assert_eq!(plan.token_env.as_deref(), Some(DEFAULT_HTTP_TOKEN_ENV));
    assert!(rendered.contains("Sigil HTTP/SSE adapter"));
    assert!(rendered.contains("bind: 127.0.0.1:0"));
    assert!(rendered.contains("auth: bearer token from SIGIL_HTTP_TOKEN"));
    assert!(rendered.contains("status: listening; press Ctrl-C for graceful shutdown"));
    assert!(!rendered.contains("secret-token"));
    Ok(())
}

#[test]
fn serve_startup_json_is_one_line_secret_free_server_metadata() -> Result<()> {
    let info = sigil_http::HttpServerInfo::new(
        "workspace-1",
        SocketAddr::from(([127, 0, 0, 1], 43123)),
        true,
    );

    let rendered = render_serve_startup_json(&info)?;
    let decoded: sigil_http::HttpServerInfo = serde_json::from_str(rendered.trim_end())?;

    assert_eq!(rendered.lines().count(), 1);
    assert_eq!(decoded, info);
    assert_eq!(
        decoded.schema_version,
        sigil_http::HTTP_SERVER_INFO_SCHEMA_VERSION
    );
    assert_eq!(decoded.bind_addr, "127.0.0.1:43123");
    assert!(decoded.capabilities.durable_session_reopen);
    assert!(decoded.capabilities.bounded_transcript_replay);
    assert!(decoded.capabilities.support_diagnostics);
    assert!(!rendered.contains(DEFAULT_HTTP_TOKEN_ENV));
    assert!(!rendered.contains("secret-token"));
    assert!(!rendered.contains("session_log_path"));
    Ok(())
}

#[tokio::test]
async fn serve_owner_channel_reports_eof_and_reaps_its_reader() -> Result<()> {
    let mut watcher = ServeOwnerChannelWatcher::spawn(std::io::Cursor::new(b"owner".to_vec()))?;

    tokio::time::timeout(std::time::Duration::from_secs(1), watcher.wait()).await?;
    watcher.reap_if_finished()?;
    Ok(())
}

#[test]
fn serve_startup_plan_rejects_disabled_auth_and_renders_token_env_fallback() -> Result<()> {
    let disabled = build_serve_startup_plan(
        ServeOptions {
            no_token: true,
            ..default_serve_options()
        },
        None,
    )
    .expect_err("V1 should reject disabled bearer authentication");
    let fallback = ServeStartupPlan {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        token_required: true,
        token_env: None,
    };

    assert!(disabled.to_string().contains("requires bearer token"));
    assert!(
        render_serve_startup_plan(&fallback).contains("auth: bearer token from SIGIL_HTTP_TOKEN")
    );
    Ok(())
}

#[test]
fn doctor_command_renders_report_for_missing_config() -> Result<()> {
    let workspace = create_test_workspace("doctor-command");

    super::doctor_command(
        &workspace.join("missing.toml"),
        &workspace,
        DoctorOutput::Text,
    )
}

#[test]
fn doctor_command_report_includes_appearance_warnings() -> Result<()> {
    let workspace = create_test_workspace("doctor-appearance");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, "https://example.com")?;
    let mut config = fs::read_to_string(&config_path)?;
    config.push_str(
        r##"
[appearance.colors]
surface_base = "#101010"
text_primary = "#101010"
"##,
    );
    fs::write(&config_path, config)?;

    let output = render_cli_doctor_report(&config_path, &workspace);

    assert!(output.contains("[warn] appearance:contrast:text-base"));
    assert!(output.contains("text_primary on surface_base"));
    assert!(output.contains("summary: warn"));
    Ok(())
}

#[tokio::test]
async fn drain_provider_stream_handles_visible_and_ignored_chunks() -> Result<()> {
    let mut stream = boxed_chunk_stream(vec![
        Ok(ProviderChunk::TextDelta("hello".to_owned())),
        Ok(ProviderChunk::ReasoningDelta("plan".to_owned())),
        Ok(ProviderChunk::ReasoningSummaryDelta("summary".to_owned())),
        Ok(ProviderChunk::ToolCallStart {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
        }),
        Ok(ProviderChunk::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: "{}".to_owned(),
        }),
        Ok(ProviderChunk::Usage(UsageStats {
            prompt_tokens: 3,
            completion_tokens: 5,
            system_fingerprint: Some("fp-test".to_owned()),
            ..UsageStats::default()
        })),
        Ok(ProviderChunk::Done),
    ]);

    drain_provider_stream(&mut stream).await
}

#[tokio::test]
async fn drain_provider_stream_propagates_chunk_errors() {
    let mut stream = boxed_chunk_stream(vec![Err(anyhow!("stream failed"))]);

    let error = drain_provider_stream(&mut stream)
        .await
        .expect_err("stream errors must be propagated");

    assert!(error.to_string().contains("stream failed"));
}

#[test]
fn stdout_event_handler_accepts_all_visible_event_variants() -> Result<()> {
    let mut handler = StdoutEventHandler;
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    };
    let spec = ToolSpec {
        name: "read_file".to_owned(),
        description: "Read file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::Optional,
    };

    handler.handle(sigil_kernel::RunEvent::TextDelta("text".to_owned()))?;
    handler.handle(sigil_kernel::RunEvent::ReasoningDelta(
        "reasoning".to_owned(),
    ))?;
    handler.handle(sigil_kernel::RunEvent::ToolCallStarted(call.clone()))?;
    handler.handle(sigil_kernel::RunEvent::ToolCallArgsDelta {
        id: call.id.clone(),
        delta: "{}".to_owned(),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolCallCompleted(call.clone()))?;
    handler.handle(sigil_kernel::RunEvent::ToolApprovalRequested {
        call: call.clone(),
        spec,
        subjects: vec![ToolSubject::path("README.md", "README.md")],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::Read,
        risk: sigil_kernel::PermissionRisk::Low,
        subject_zones: vec![sigil_kernel::PathTrustZone::WorkspaceSource],
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(ToolPreview {
            title: "Preview".to_owned(),
            summary: "read README".to_owned(),
            body: String::new(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: Vec::new(),
        }),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolApprovalResolved {
        call_id: call.id.clone(),
        approved: false,
        reason: Some("denied by test".to_owned()),
    })?;
    handler.handle(sigil_kernel::RunEvent::ToolResult(ToolResult::error(
        call.id,
        call.name,
        ToolErrorKind::Internal,
        "failed",
    )))?;
    handler.handle(sigil_kernel::RunEvent::Usage(UsageStats {
        prompt_tokens: 1,
        completion_tokens: 2,
        system_fingerprint: Some("fp-test".to_owned()),
        ..UsageStats::default()
    }))?;
    handler.handle(sigil_kernel::RunEvent::Notice("notice".to_owned()))?;
    handler.handle(sigil_kernel::RunEvent::AssistantMessage(
        ModelMessage::assistant(Some("assistant".to_owned()), Vec::new()),
    ))?;
    handler.handle(sigil_kernel::RunEvent::ToolResult(ToolResult::ok(
        "call-ok",
        "read_file",
        "ok",
        ToolResultMeta::default(),
    )))?;

    Ok(())
}

#[tokio::test]
async fn prefix_command_streams_against_configured_provider() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"prefixed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("prefix-command");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, &server)?;

    super::prefix_command(
        &config_path,
        &workspace,
        "write code".to_owned(),
        "```rust\n".to_owned(),
        vec!["```".to_owned()],
        Some("deepseek-v4-flash".to_owned()),
    )
    .await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded prefix request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"prefix\":true"));
    assert!(raw_request.contains("```rust"));
    assert!(raw_request.contains("\"user_id\":\"workspace-"));
    assert!(!raw_request.contains("local-user"));
    Ok(())
}

#[tokio::test]
async fn fim_command_streams_against_configured_provider() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"text\":\"middle\",\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"prompt_cache_hit_tokens\":2,\"prompt_cache_miss_tokens\":5},\"system_fingerprint\":\"fp-fim\"}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("fim-command");
    let config_path = workspace.join("sigil.toml");
    write_test_config(&config_path, &server)?;

    super::fim_command(
        &config_path,
        "fn main() {\n".to_owned(),
        "\n}\n".to_owned(),
        vec!["STOP".to_owned()],
        Some("deepseek-v4-pro".to_owned()),
        Some(32),
    )
    .await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded fim request");
    assert!(raw_request.contains("POST /completions"));
    assert!(raw_request.contains("\"suffix\":\"\\n}\\n\""));
    assert!(raw_request.contains("\"max_tokens\":32"));
    Ok(())
}

#[tokio::test]
async fn run_command_creates_session_log_in_user_state() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello from agent\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), Arc::clone(&responses)).await?;
    let workspace = create_test_workspace("run-command");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, &server)?;

    super::run_command(&config_path, &workspace, "Say hi".to_owned(), None, None).await?;

    let raw_request = requests
        .lock()
        .expect("requests poisoned")
        .pop_front()
        .expect("expected recorded run request");
    assert!(raw_request.contains("POST /chat/completions"));
    assert!(raw_request.contains("\"Say hi\""));

    let root_config = RootConfig::load(&config_path)?;
    let paths =
        sigil_runtime::resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace);
    let session_dir = paths.session_log_dir;
    let entries = fs::read_dir(&session_dir)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "run_command should create one session log"
    );
    let session_path = entries[0].path();
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    let session_contents = fs::read_to_string(session_path)?;
    assert!(session_contents.contains("Say hi"));
    assert!(session_contents.contains("hello from agent"));
    assert!(session_contents.contains("\"event_type\":\"session_entry_recorded\""));
    let provider_attempt = session_contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["event_type"] == "provider_physical_attempt_started")
        .expect("run should persist a provider physical-attempt start");
    let logical_run_id = provider_attempt["payload"]["logical_run_id"]
        .as_str()
        .expect("provider attempt should carry the application run id");
    assert!(uuid::Uuid::parse_str(logical_run_id).is_ok());
    assert!(!logical_run_id.starts_with("agent-run-"));
    Ok(())
}

#[tokio::test]
async fn run_json_emits_exactly_one_terminal_result_record() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"json answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), responses).await?;
    let workspace = create_test_workspace("run-json");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, &server)?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &config_path,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Json,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::Success);
    let lines = String::from_utf8(stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["record_type"], "result");
    assert_eq!(lines[0]["protocol_version"], 1);
    assert_eq!(lines[0]["result"]["status"], "succeeded");
    assert_eq!(lines[0]["result"]["final_text"], "json answer");
    assert!(
        std::path::Path::new(
            lines[0]["result"]["session_log_path"]
                .as_str()
                .expect("result must expose the durable session path")
        )
        .exists()
    );
    Ok(())
}

#[tokio::test]
async fn run_jsonl_emits_ordered_events_then_one_terminal_result() -> Result<()> {
    let requests = Arc::new(Mutex::new(VecDeque::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from(vec![http_response(
        200,
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"jsonl answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )])));
    let server = spawn_recording_server(Arc::clone(&requests), responses).await?;
    let workspace = create_test_workspace("run-jsonl");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, &server)?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &config_path,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Jsonl,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::Success);
    let lines = String::from_utf8(stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert!(lines.len() >= 3);
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .all(|record| record["record_type"] == "event")
    );
    assert_eq!(
        lines.last().expect("terminal record")["record_type"],
        "result"
    );
    let sequences = lines[..lines.len() - 1]
        .iter()
        .map(|record| {
            record["event"]["sequence"]
                .as_u64()
                .expect("event sequence")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequences,
        (1..=u64::try_from(sequences.len())?).collect::<Vec<_>>()
    );
    assert_eq!(
        lines.last().expect("terminal record")["result"]["final_text"],
        "jsonl answer"
    );
    Ok(())
}

#[tokio::test]
async fn run_json_classifies_missing_config_without_leaking_raw_source() -> Result<()> {
    let workspace = create_test_workspace("run-json-invalid-config");
    let missing = workspace.join("missing.toml");
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_writer(
        &missing,
        &workspace,
        "Say hi".to_owned(),
        RunOutput::Json,
        &mut stdout,
    )
    .await;

    assert_eq!(exit, MachineExitCode::InvalidInput);
    let record: serde_json::Value = serde_json::from_slice(&stdout)?;
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["error"]["code"], "model_route_not_configured");
    assert_eq!(record["error"]["message"], "model route is not configured");
    assert!(!String::from_utf8(stdout)?.contains("missing.toml"));
    Ok(())
}

#[tokio::test]
async fn run_json_cancellation_during_preparation_emits_error_and_exit_130() -> Result<()> {
    let workspace = create_test_workspace("run-json-cancelled");
    let config_path = workspace.join("sigil.toml");
    write_application_run_test_config(&config_path, "http://127.0.0.1:9")?;
    let mut stdout = Vec::new();

    let exit = run_machine_command_with_cancellation(
        &config_path,
        &workspace,
        "Wait".to_owned(),
        RunOutput::Json,
        &mut stdout,
        std::future::ready(Ok(())),
    )
    .await;

    assert_eq!(exit, MachineExitCode::Cancelled);
    let record: serde_json::Value = serde_json::from_slice(&stdout)?;
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["error"]["code"], "cancelled");
    assert_eq!(
        record["error"]["message"],
        "application run was cancelled before startup completed"
    );
    assert!(!workspace.join("state").exists());
    Ok(())
}

fn create_test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("sigil-tests-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).expect("test workspace should create");
    path
}

fn default_serve_options() -> ServeOptions {
    ServeOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 0,
        token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        no_token: false,
        startup_output: ServeStartupOutput::Text,
        shutdown_on_stdin_close: false,
    }
}

fn write_test_config(path: &std::path::Path, base_url: &str) -> Result<()> {
    let workspace = path
        .parent()
        .ok_or_else(|| anyhow!("test config path should have a parent"))?;
    let state_root = workspace.join("state");
    let cache_root = workspace.join("cache");
    let config = format!(
        r#"[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[providers.deepseek]
base_url = "{base_url}"
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
fim_model = "deepseek-v4-pro"
api_key = "test-key"
strict_tools_mode = "auto"
"#,
        state_root.display(),
        cache_root.display()
    );
    fs::write(path, config)?;
    Ok(())
}

fn write_application_run_test_config(path: &std::path::Path, base_url: &str) -> Result<()> {
    let workspace = path
        .parent()
        .ok_or_else(|| anyhow!("test config path should have a parent"))?;
    let state_root = workspace.join("state");
    let cache_root = workspace.join("cache");
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#,
        state_root.display(),
        cache_root.display()
    );
    fs::write(path, config)?;
    Ok(())
}

async fn spawn_recording_server(
    requests: Arc<Mutex<VecDeque<String>>>,
    responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let requests = Arc::clone(&requests);
            let responses = Arc::clone(&responses);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let bytes = socket.read(&mut buffer).await.unwrap_or(0);
                requests
                    .lock()
                    .expect("requests poisoned")
                    .push_back(String::from_utf8_lossy(&buffer[..bytes]).to_string());
                let response = responses
                    .lock()
                    .expect("responses poisoned")
                    .pop_front()
                    .unwrap_or_else(|| http_response(500, "text/plain", "missing fixture"));
                let _ = socket.write_all(&response).await;
            });
        }
    });
    Ok(format!("http://{}", address))
}

fn http_response(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}
