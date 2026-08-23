use std::fs;

use anyhow::Result;
use sigil_kernel::{ApprovalMode, ToolCall, ToolContext, ToolOperation, ToolRegistry};

use super::*;

fn test_store(root: &Path, workspace_id: &str) -> WritableMemoryStore {
    WritableMemoryStore {
        user_preferences: MemoryScopeStore::new(
            root.join("user-preferences"),
            WritableMemoryScope::UserPreference,
            None,
        ),
        project_facts: MemoryScopeStore::new(
            root.join("workspaces")
                .join(workspace_id)
                .join("project-facts"),
            WritableMemoryScope::ProjectFact,
            Some(workspace_id.to_owned()),
        ),
        managed_writer: None,
    }
}

fn source(suffix: &str) -> MemoryWriteSource {
    MemoryWriteSource {
        session_scope_id: format!("session-{suffix}"),
        logical_run_id: format!("run-{suffix}"),
        tool_call_id: format!("call-{suffix}"),
    }
}

#[test]
fn remember_is_durable_idempotent_and_keeps_content_out_of_the_journal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    let statement = "Prefer concise Chinese technical answers.";

    let first = store.remember(
        WritableMemoryScope::UserPreference,
        statement,
        source("first"),
    )?;
    let duplicate = store.remember(
        WritableMemoryScope::UserPreference,
        statement,
        source("duplicate"),
    )?;

    assert!(first.durable);
    assert!(first.created);
    assert!(!duplicate.created);
    assert_eq!(duplicate.memory_id, first.memory_id);
    assert_eq!(duplicate.version, 1);
    let entries = store.inspect(Some(WritableMemoryScope::UserPreference), 10)?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].statement, statement);

    let journal = fs::read_to_string(
        temp.path()
            .join("user-preferences")
            .join("journal-v1.jsonl"),
    )?;
    assert!(!journal.contains(statement));
    assert!(!journal.contains(&sha256_hex(statement.as_bytes())));
    assert!(journal.contains(&first.memory_id));
    Ok(())
}

#[test]
fn project_memory_is_workspace_scoped_while_user_preferences_are_shared() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_a = test_store(temp.path(), "workspace-a");
    let workspace_b = test_store(temp.path(), "workspace-b");

    workspace_a.remember(
        WritableMemoryScope::UserPreference,
        "Prefer findings before implementation details.",
        source("preference"),
    )?;
    workspace_a.remember(
        WritableMemoryScope::ProjectFact,
        "Sigil uses Conventional Commit messages.",
        source("project"),
    )?;

    assert_eq!(
        workspace_b
            .inspect(Some(WritableMemoryScope::UserPreference), 10)?
            .len(),
        1
    );
    assert!(
        workspace_b
            .inspect(Some(WritableMemoryScope::ProjectFact), 10)?
            .is_empty()
    );
    assert!(
        workspace_b
            .retrieve_context("What commit convention does Sigil use?")?
            .items
            .iter()
            .all(|item| item.source != ContextSource::ProjectMemory)
    );
    Ok(())
}

#[test]
fn retrieval_supports_cjk_queries_without_claiming_provider_use() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    let receipt = store.remember(
        WritableMemoryScope::ProjectFact,
        "项目提交规范要求使用 Conventional Commit。",
        source("remember"),
    )?;

    let context = store.retrieve_context("这个项目的提交规范是什么？")?;

    assert_eq!(context.items.len(), 1);
    assert_eq!(context.items[0].source, ContextSource::ProjectMemory);
    assert!(
        context
            .snippets
            .values()
            .any(|snippet| snippet.contains("Conventional Commit"))
    );
    let journal = fs::read_to_string(
        temp.path()
            .join("workspaces")
            .join("workspace-a")
            .join("project-facts")
            .join("journal-v1.jsonl"),
    )?;
    assert!(!journal.contains("\"action\":\"retrieved\""));
    assert!(journal.contains(&receipt.memory_id));
    assert!(!journal.contains("Conventional Commit"));
    let admission: MemoryJournalRecordV1 =
        serde_json::from_str(journal.lines().next().expect("admission"))?;
    assert_eq!(
        context.items[0].source_event_id.as_deref(),
        Some(admission.event_id.as_str())
    );
    Ok(())
}

#[test]
fn forget_tombstones_then_physically_deletes_and_survives_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    let receipt = store.remember(
        WritableMemoryScope::ProjectFact,
        "The release gate is cargo test.",
        source("remember"),
    )?;
    let sidecar = temp
        .path()
        .join("workspaces")
        .join("workspace-a")
        .join("project-facts")
        .join("entries")
        .join(format!("{}.json", receipt.memory_id));
    assert!(sidecar.is_file());

    let forgotten = store.forget(&receipt.memory_id, source("forget"))?;
    let repeated = store.forget(&receipt.memory_id, source("forget-again"))?;

    assert!(forgotten.durable);
    assert!(forgotten.tombstoned);
    assert!(forgotten.physical_deleted);
    assert!(!forgotten.prior_provider_context_retracted);
    assert!(repeated.durable);
    assert!(repeated.physical_deleted);
    assert!(!sidecar.exists());
    let restarted = test_store(temp.path(), "workspace-a");
    assert!(restarted.inspect(None, 10)?.is_empty());
    assert!(
        restarted
            .retrieve_context("What is the release gate?")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn forget_preview_can_recover_an_already_deleted_lineage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    let receipt = store.remember(
        WritableMemoryScope::ProjectFact,
        "The project release gate is cargo test.",
        source("remember"),
    )?;
    store.forget(&receipt.memory_id, source("forget"))?;
    let tool = ForgetMemoryTool {
        store: Arc::new(store),
    };

    let preview = tool
        .preview(
            ToolContext::new(temp.path(), 5),
            serde_json::json!({ "memory_id": receipt.memory_id }),
        )
        .await?
        .expect("forget preview");

    assert!(preview.title.contains("Finish forgetting"));
    assert!(preview.body.contains("already tombstoned or deleted"));
    Ok(())
}

#[test]
fn secret_like_statements_are_rejected_before_any_store_is_created() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = test_store(temp.path(), "workspace-a");

    let error = store
        .remember(
            WritableMemoryScope::UserPreference,
            "api_key = super-secret-value",
            source("secret"),
        )
        .expect_err("secret-like memory must fail closed");

    assert!(error.to_string().contains("credential material"));
    assert!(!temp.path().join("user-preferences").exists());
}

#[test]
fn common_and_high_entropy_credential_formats_are_rejected() {
    for statement in [
        "github_pat_11AA22BB33CC44DD55EE66FF",
        "xoxb-123456789012-ABCDEFGHIJKL",
        "AIzaSyA1234567890abcdefghijklmnop",
        "mF9qR2vL7xK4pN8sT1wY6zC3dH5jB0uG",
        "authorization: Bearer 1234567890abcdef",
    ] {
        validate_memory_statement(statement)
            .expect_err("credential-shaped durable memory must fail closed");
    }
    assert!(validate_memory_statement("The project uses SHA-256 content hashes.").is_ok());
}

#[test]
fn orphan_sidecars_are_reconciled_without_hiding_active_memory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    let receipt = store.remember(
        WritableMemoryScope::UserPreference,
        "Prefer concise status updates.",
        source("remember"),
    )?;
    let entries_dir = temp.path().join("user-preferences").join("entries");
    let admitted = entries_dir.join(format!("{}.json", receipt.memory_id));
    let orphan = entries_dir.join(format!("memory-{}.json", uuid::Uuid::new_v4()));
    fs::copy(&admitted, &orphan)?;

    let entries = store.inspect(Some(WritableMemoryScope::UserPreference), 10)?;

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].memory_id, receipt.memory_id);
    assert!(!orphan.exists());
    Ok(())
}

#[test]
fn journal_append_bounds_reject_the_record_that_would_cross_a_limit() {
    assert!(
        validate_journal_append_bounds(
            MEMORY_JOURNAL_MAX_BYTES as usize - 4,
            MEMORY_JOURNAL_MAX_RECORDS - 1,
            4,
        )
        .is_ok()
    );
    assert!(
        validate_journal_append_bounds(
            MEMORY_JOURNAL_MAX_BYTES as usize - 4,
            MEMORY_JOURNAL_MAX_RECORDS - 1,
            5,
        )
        .expect_err("byte overflow must be rejected")
        .to_string()
        .contains("size limit")
    );
    assert!(
        validate_journal_append_bounds(0, MEMORY_JOURNAL_MAX_RECORDS, 1)
            .expect_err("record overflow must be rejected")
            .to_string()
            .contains("record limit")
    );
}

#[test]
fn malformed_journal_fails_closed_instead_of_retrieving_sidecars() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = test_store(temp.path(), "workspace-a");
    store.remember(
        WritableMemoryScope::UserPreference,
        "Prefer Chinese responses.",
        source("remember"),
    )?;
    fs::write(
        temp.path()
            .join("user-preferences")
            .join("journal-v1.jsonl"),
        "not-json\n",
    )?;

    let error = store
        .retrieve_context("preferences")
        .expect_err("corrupt lifecycle evidence must fail closed");

    assert!(error.to_string().contains("failed to decode"));
    Ok(())
}

#[test]
fn registration_exposes_explicit_memory_lifecycle_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut registry = ToolRegistry::new();
    register_writable_memory_tools(&mut registry, test_store(temp.path(), "workspace-a"));

    let specs = registry.specs();
    let names = specs
        .iter()
        .map(|spec| spec.name.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from([
            FORGET_MEMORY_TOOL_NAME.to_owned(),
            INSPECT_MEMORY_TOOL_NAME.to_owned(),
            REMEMBER_PROJECT_FACT_TOOL_NAME.to_owned(),
            REMEMBER_USER_PREFERENCE_TOOL_NAME.to_owned(),
        ])
    );
    for spec in specs
        .iter()
        .filter(|spec| spec.name.starts_with("remember_"))
    {
        assert!(spec.description.contains("user's meaning"));
        assert!(spec.description.contains("not keyword matching"));
        assert!(spec.description.contains("durable receipt"));
    }
    for expected in sigil_kernel::writable_memory_route_tool_specs() {
        let actual = registry
            .spec_for(&expected.name)
            .expect("canonical remember tool should be registered");
        assert_eq!(
            serde_json::to_value(actual).expect("serialize actual spec"),
            serde_json::to_value(expected).expect("serialize canonical spec")
        );
    }
}

#[test]
fn remember_tools_require_preview_and_default_to_ask() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    register_writable_memory_tools(&mut registry, test_store(temp.path(), "workspace-a"));
    let call = ToolCall {
        id: "remember-call".to_owned(),
        name: REMEMBER_PROJECT_FACT_TOOL_NAME.to_owned(),
        args_json: serde_json::json!({
            "statement": "This project uses Conventional Commit messages."
        })
        .to_string(),
    };

    let spec = registry
        .spec_for(REMEMBER_PROJECT_FACT_TOOL_NAME)
        .expect("remember tool spec");
    let plan = registry.permission_plan(&ToolContext::new(temp.path(), 5), &call)?;

    assert_eq!(spec.preview, sigil_kernel::ToolPreviewCapability::Required);
    assert_eq!(plan.operation, ToolOperation::RememberMemory);
    assert_eq!(plan.tool_default_mode, Some(ApprovalMode::Ask));

    let forget_call = ToolCall {
        id: "forget-call".to_owned(),
        name: FORGET_MEMORY_TOOL_NAME.to_owned(),
        args_json: serde_json::json!({
            "memory_id": "memory-00000000-0000-4000-8000-000000000000"
        })
        .to_string(),
    };
    let forget_plan = registry.permission_plan(&ToolContext::new(temp.path(), 5), &forget_call)?;
    assert_eq!(forget_plan.operation, ToolOperation::ForgetMemory);
    assert_eq!(forget_plan.tool_default_mode, Some(ApprovalMode::Ask));
    assert_eq!(
        forget_plan
            .semantic_scope
            .as_ref()
            .and_then(|scope| scope.qualifiers.get("scope"))
            .map(String::as_str),
        Some("any")
    );
    Ok(())
}

#[test]
fn managed_writer_round_trips_both_memory_scopes_under_admitted_namespaces() -> Result<()> {
    use crate::managed_storage_writer::{ManagedStorageWriterAdapterV1, memory_grants};
    use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
    use sigil_kernel::managed_storage::ManagedStorageServiceV1;
    use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash};
    use sigil_resource_authority::storage::{
        AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
    };

    let temp = tempfile::tempdir()?;
    let anchor = temp.path().join("state");
    std::fs::create_dir_all(&anchor)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o700))?;
    }

    let mut table = AuthorityStorageGrantTableV1::new();
    for grant in memory_grants(0x76) {
        table.register(grant)?;
    }
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0x68; 32]),
            },
        ));
    let broker = std::sync::Arc::new(KernelCapabilityBrokerV1::new());
    let writer = std::sync::Arc::new(ManagedStorageWriterAdapterV1::with_storage_issuer(
        service,
        anchor.clone(),
        CanonicalHash::from_bytes([0x69; 32]),
        broker,
    ));
    let store = WritableMemoryStore::with_managed_writer("workspace-managed", Arc::clone(&writer))?;

    let preference = store.remember(
        WritableMemoryScope::UserPreference,
        "user likes concise Chinese replies",
        source("preference"),
    )?;
    let fact = store.remember(
        WritableMemoryScope::ProjectFact,
        "managed memory rounds trip through the composed writer",
        source("fact"),
    )?;
    assert_ne!(preference.memory_id, fact.memory_id);

    let inspected = store.inspect(Some(WritableMemoryScope::UserPreference), 16)?;
    assert!(
        inspected
            .iter()
            .any(|entry| entry.statement.contains("concise Chinese")),
        "managed user-preferences namespace must be readable: {:?}",
        inspected
    );
    let facts = store.inspect(Some(WritableMemoryScope::ProjectFact), 16)?;
    assert!(
        facts
            .iter()
            .any(|entry| entry.statement.contains("composed writer")),
        "managed project-facts namespace must be readable: {:?}",
        facts
    );

    // The physical leaves are authority-declared named namespaces, never caller paths.
    assert!(
        anchor
            .join("managed")
            .join("durable-memory")
            .join("user-preferences")
            .is_dir()
    );
    assert!(
        anchor
            .join("managed")
            .join("durable-memory")
            .join("project-facts")
            .is_dir()
    );
    Ok(())
}
