use std::fs;

use anyhow::Result;

use crate::MemoryConfig;

use super::{inspect_memory_documents, materialize_memory};

#[test]
fn memory_loader_walks_root_files_and_imports_in_stable_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("AGENTS.md"), "root\n@docs/guide.md\n")?;
    fs::create_dir_all(temp.path().join("docs"))?;
    fs::write(temp.path().join("docs/guide.md"), "guide\n")?;
    fs::write(temp.path().join("TERMQUILL.local.md"), "local\n")?;

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
            .is_some_and(|content| content.contains("TERMQUILL.local.md"))
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
