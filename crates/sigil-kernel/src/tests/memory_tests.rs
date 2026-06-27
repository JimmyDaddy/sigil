use std::{fs, path::Path};

use anyhow::Result;

use crate::{MemoryConfig, PrefixSnapshot};

use super::{
    apply_memory_report, inspect_memory_documents, materialize_memory, parse_memory_file,
    resolve_import_path, stable_memory_message_id,
};

#[test]
fn memory_loader_walks_root_files_and_imports_in_stable_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "root\n@docs/guide.md\n")?;
    fs::create_dir_all(temp.path().join("docs"))?;
    fs::write(temp.path().join("docs/guide.md"), "guide\n")?;
    fs::write(temp.path().join("SIGIL.local.md"), "local\n")?;

    let report = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: true })?;
    let materialized = materialize_memory(temp.path(), &MemoryConfig { enabled: true })?;

    assert_eq!(report.document_count, 3);
    assert_eq!(materialized.report.document_count, 3);
    assert_eq!(materialized.messages.len(), 4);
    assert!(
        materialized.messages[1]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("AGENTS.md"))
    );
    assert!(
        materialized.messages[2]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("docs/guide.md"))
    );
    assert!(
        materialized.messages[3]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("SIGIL.local.md"))
    );
    Ok(())
}

#[test]
fn memory_loader_rejects_import_cycles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "@docs/guide.md\n")?;
    fs::create_dir_all(temp.path().join("docs"))?;
    fs::write(temp.path().join("docs/guide.md"), "@../AGENTS.md\n")?;

    let error = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: true })
        .expect_err("expected import cycle to fail");

    assert!(error.to_string().contains("memory import cycle detected"));
    Ok(())
}

#[test]
fn memory_loader_skips_empty_documents_and_applies_report_fingerprint() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "\n")?;

    let materialized = materialize_memory(temp.path(), &MemoryConfig { enabled: true })?;

    assert_eq!(materialized.report.document_count, 1);
    assert_eq!(materialized.messages.len(), 1);

    let mut snapshot = PrefixSnapshot {
        materialized_text: "prefix".to_owned(),
        sha256: "hash".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        memory_fingerprint: "stale".to_owned(),
        tool_schema_fingerprint: "tools".to_owned(),
        skill_index_fingerprint: "skills".to_owned(),
    };
    apply_memory_report(&mut snapshot, &materialized.report);
    assert_eq!(snapshot.memory_fingerprint, materialized.report.fingerprint);
    Ok(())
}

#[test]
fn memory_loader_returns_empty_report_when_disabled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "repo rules\n")?;

    let report = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: false })?;
    let materialized = materialize_memory(temp.path(), &MemoryConfig { enabled: false })?;

    assert!(!report.enabled);
    assert_eq!(report.document_count, 0);
    assert_eq!(report.fingerprint, "none");
    assert_eq!(materialized.messages.len(), 1);
    Ok(())
}

#[test]
fn memory_loader_enabled_without_documents_keeps_base_prompt_only() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let report = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: true })?;
    let materialized = materialize_memory(temp.path(), &MemoryConfig { enabled: true })?;

    assert!(report.enabled);
    assert_eq!(report.document_count, 0);
    assert_eq!(report.fingerprint, "none");
    assert_eq!(materialized.messages.len(), 1);
    assert_eq!(materialized.messages[0].id, "system:base");
    let base_content = materialized.messages[0]
        .content
        .as_deref()
        .expect("base prompt should have content");
    let base_content_lower = base_content.to_ascii_lowercase();
    assert!(base_content.contains("AI coding agent working inside the user's workspace"));
    assert!(base_content.contains("implementation details"));
    assert!(!base_content.contains("TUI-first"));
    assert!(!base_content_lower.contains("rust"));
    assert!(
        base_content.contains("model-visible agent tools")
            && base_content.contains("explicitly asks")
            && !base_content.contains("Direct task/subagent tool calls")
            && !base_content.contains("/plan flow")
    );
    Ok(())
}

#[test]
fn memory_loader_reports_missing_workspace_root() {
    let missing_root =
        Path::new("/tmp").join(format!("sigil-memory-missing-{}", uuid::Uuid::new_v4()));

    let error = inspect_memory_documents(&missing_root, &MemoryConfig { enabled: true })
        .expect_err("missing root should fail");

    assert!(error.to_string().contains("failed to canonicalize"));
}

#[test]
fn memory_loader_skips_duplicate_imports() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("docs"))?;
    fs::write(
        temp.path().join("AGENTS.md"),
        "root\n@docs/guide.md\n@docs/guide.md\n",
    )?;
    fs::write(temp.path().join("docs/guide.md"), "guide\n")?;

    let report = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: true })?;

    assert_eq!(report.document_count, 2);
    Ok(())
}

#[test]
fn memory_loader_rejects_absolute_imports() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "@/tmp/outside.md\n")?;

    let error = inspect_memory_documents(temp.path(), &MemoryConfig { enabled: true })
        .expect_err("absolute imports should fail");

    assert!(error.to_string().contains("memory import must be relative"));
    Ok(())
}

#[test]
fn memory_loader_rejects_workspace_escape_imports() -> Result<()> {
    let parent = tempfile::tempdir()?;
    let workspace_root = parent.path().join("workspace");
    fs::create_dir_all(&workspace_root)?;
    fs::write(workspace_root.join("AGENTS.md"), "@../outside.md\n")?;
    fs::write(parent.path().join("outside.md"), "outside\n")?;

    let error = inspect_memory_documents(&workspace_root, &MemoryConfig { enabled: true })
        .expect_err("imports outside the workspace should fail");

    assert!(error.to_string().contains("escapes workspace root"));
    Ok(())
}

#[test]
fn memory_parser_keeps_empty_import_markers_as_content() {
    let (content, imports) = parse_memory_file("alpha\n@\n@   \n@ docs/guide.md\nbeta");

    assert_eq!(content, "alpha\n@\n@   \nbeta");
    assert_eq!(imports, vec!["docs/guide.md"]);
}

#[test]
fn memory_import_path_resolution_accepts_relative_and_rejects_absolute() {
    let base = Path::new("/workspace/docs");

    let relative = resolve_import_path(base, "../AGENTS.md").expect("relative import should work");
    assert_eq!(relative, Path::new("/workspace/docs/../AGENTS.md"));

    let error =
        resolve_import_path(base, "/tmp/outside.md").expect_err("absolute import should fail");
    assert!(error.to_string().contains("memory import must be relative"));
}

#[test]
fn stable_memory_message_id_changes_with_path_or_content() {
    let original = stable_memory_message_id("AGENTS.md", "rules");
    let same = stable_memory_message_id("AGENTS.md", "rules");
    let changed_path = stable_memory_message_id("docs/guide.md", "rules");
    let changed_content = stable_memory_message_id("AGENTS.md", "other rules");

    assert_eq!(original, same);
    assert_ne!(original, changed_path);
    assert_ne!(original, changed_content);
    assert!(original.starts_with("memory:"));
}
