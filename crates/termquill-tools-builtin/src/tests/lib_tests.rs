use std::fs;

use anyhow::Result;
use serde_json::json;
use termquill_kernel::{Tool, ToolContext, ToolRegistry};

use super::{
    BashTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool, WriteFileTool,
    register_builtin_tools,
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[tokio::test]
async fn read_and_edit_file_tool_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let read = ReadFileTool
        .execute(ctx.clone(), "1".to_owned(), json!({ "path": "note.txt" }))
        .await?;
    assert_eq!(read.content, "hello old");
    EditFileTool
        .execute(
            ctx.clone(),
            "2".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await?;
    assert_eq!(fs::read_to_string(file)?, "hello new");
    Ok(())
}

#[tokio::test]
async fn write_file_preview_contains_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "alpha\nbeta\n")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let preview = WriteFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "content": "alpha\nbeta\ngamma\n" }),
        )
        .await?
        .expect("expected preview");
    assert!(preview.body.contains("--- current/note.txt"));
    assert!(preview.body.contains("+++ proposed/note.txt"));
    assert!(preview.body.contains("+gamma"));
    Ok(())
}

#[tokio::test]
async fn write_file_preview_errors_for_unreadable_existing_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, [0xff_u8, 0xfe, 0xfd])?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let error = WriteFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "content": "hello\nworld\n" }),
        )
        .await
        .expect_err("expected preview generation to surface the read failure");
    assert!(error.to_string().contains("failed to read"));
    Ok(())
}

#[tokio::test]
async fn edit_file_preview_contains_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old\n")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let preview = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await?
        .expect("expected preview");
    assert!(preview.body.contains("-hello old"));
    assert!(preview.body.contains("+hello new"));
    Ok(())
}

#[test]
fn register_builtin_tools_registers_multiple_tools() {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    assert!(registry.specs().len() >= 7);
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_rejects_symlink_escape() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = ReadFileTool
        .execute(ctx, "read".to_owned(), json!({ "path": "leak.txt" }))
        .await
        .expect_err("expected symlink escape to be rejected");

    assert!(error.to_string().contains("escapes workspace"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_rejects_existing_symlink_escape() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "leak.txt", "content": "changed" }),
        )
        .await
        .expect_err("expected symlink escape to be rejected");

    assert!(error.to_string().contains("escapes workspace"));
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_rejects_symlink_parent_escape_for_new_file() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "outside-dir/new.txt", "content": "changed" }),
        )
        .await
        .expect_err("expected symlink parent escape to be rejected");

    assert!(error.to_string().contains("escapes workspace"));
    assert!(!outside.path().join("new.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn edit_file_rejects_symlink_escape() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "hello old")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = EditFileTool
        .execute(
            ctx,
            "edit".to_owned(),
            json!({ "path": "leak.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("expected symlink escape to be rejected");

    assert!(error.to_string().contains("escapes workspace"));
    assert_eq!(fs::read_to_string(outside_file)?, "hello old");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn list_and_grep_reject_external_symlink_roots() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let list_error = ListTool
        .execute(
            ctx.clone(),
            "list".to_owned(),
            json!({ "path": "outside-dir", "recursive": true }),
        )
        .await
        .expect_err("expected list symlink root to be rejected");
    let grep_error = GrepTool
        .execute(
            ctx,
            "grep".to_owned(),
            json!({ "path": "outside-dir", "pattern": "secret" }),
        )
        .await
        .expect_err("expected grep symlink root to be rejected");

    assert!(list_error.to_string().contains("escapes workspace"));
    assert!(grep_error.to_string().contains("escapes workspace"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn glob_does_not_traverse_external_symlink_targets() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    fs::write(workspace.path().join("visible.txt"), "visible")?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = GlobTool
        .execute(ctx, "glob".to_owned(), json!({ "pattern": "**/*.txt" }))
        .await?;

    assert!(result.content.contains("visible.txt"));
    assert!(!result.content.contains("secret.txt"));
    Ok(())
}

#[tokio::test]
async fn bash_tool_timeout_surfaces_structured_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = BashTool
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "sleep 2", "timeout_secs": 1 }),
        )
        .await
        .expect_err("expected timeout to be surfaced");

    assert!(error.to_string().contains("bash command timed out"));
    Ok(())
}
