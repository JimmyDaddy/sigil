use anyhow::Result;
use sigil_kernel::{FileChangeRef, SourcedFact, TaskMemoryV1};

use super::context_items_from_task_memory;

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
