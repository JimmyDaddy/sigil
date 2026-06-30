use std::fs;

use anyhow::Result;
use sigil_kernel::{
    ContextInclusionReason, ContextSource, FileChangeRef, PluginHookContextOptions,
    PluginHookOutputEnvelope, PluginHookOutputStream, RedactionState, SourcedFact, TaskMemoryV1,
};

use super::{
    context_candidates_from_repo_query, context_items_from_plugin_hook_output,
    context_items_from_task_memory,
};

fn runtime_task_memory() -> TaskMemoryV1 {
    TaskMemoryV1 {
        memory_id: "runtime-memory".to_owned(),
        branch_id: None,
        valid_for_snapshot: "snapshot-runtime".to_owned(),
        supersedes: None,
        source_event_ids: vec!["event-objective".to_owned()],
        objective: "Keep context provenance inspectable".to_owned(),
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
    let output = PluginHookOutputEnvelope {
        execution_id: "hook-exec-runtime".to_owned(),
        plugin_id: "repo-review".to_owned(),
        hook_id: "context-rules".to_owned(),
        stdout: PluginHookOutputStream {
            content: "Prefer the existing context V0 adapter.".to_owned(),
            total_bytes: 39,
            returned_bytes: 39,
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
    };

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
