use std::fs;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    Tool, ToolAccess, ToolContext, ToolErrorKind, ToolPreviewCapability, ToolRegistry,
    ToolResultStatus, ToolSubjectScope,
};

use super::{
    BashTool, DeleteFileTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool,
    WriteFileTool, register_builtin_tools,
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
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("+gamma"));
    Ok(())
}

#[tokio::test]
async fn write_file_preview_for_new_file_contains_create_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let preview = WriteFileTool
        .preview(ctx, json!({ "path": "new-note.txt", "content": "hello\n" }))
        .await?
        .expect("expected preview");

    assert_eq!(preview.changed_files, vec!["new-note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "new-note.txt");
    assert!(
        preview.file_diffs[0]
            .diff
            .contains("--- current/new-note.txt")
    );
    assert!(
        preview.file_diffs[0]
            .diff
            .contains("+++ proposed/new-note.txt")
    );
    assert!(preview.file_diffs[0].diff.contains("+hello"));
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
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("+hello new"));
    Ok(())
}

#[tokio::test]
async fn delete_file_preview_contains_delete_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.txt"), "alpha\nbeta\n")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let preview = DeleteFileTool
        .preview(ctx, json!({ "path": "note.txt" }))
        .await?
        .expect("expected preview");

    assert_eq!(preview.title, "Delete note.txt");
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("--- current/note.txt"));
    assert!(preview.file_diffs[0].diff.contains("+++ proposed/note.txt"));
    assert!(preview.file_diffs[0].diff.contains("-alpha"));
    assert!(preview.file_diffs[0].diff.contains("-beta"));
    Ok(())
}

#[tokio::test]
async fn delete_file_execute_deletes_regular_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "alpha\nbeta\n")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "note.txt" }))
        .await?;

    assert!(!file.exists());
    assert_eq!(result.tool_name, "delete_file");
    assert_eq!(result.metadata.changed_files, vec!["note.txt"]);
    assert_eq!(result.metadata.bytes, Some("alpha\nbeta\n".len() as u64));
    assert_eq!(result.metadata.details["action"], "delete");
    let model_content = result.to_model_content();
    assert!(model_content.contains("deleted"));
    assert!(!model_content.contains("-alpha"));
    assert!(!model_content.contains("file_diffs"));
    Ok(())
}

#[tokio::test]
async fn delete_file_errors_for_missing_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "missing.txt" }))
        .await
        .expect_err("expected missing file to fail");

    assert!(error.to_string().contains("failed to inspect"));
    Ok(())
}

#[tokio::test]
async fn delete_file_errors_for_directory_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("dir"))?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "dir" }))
        .await
        .expect_err("expected directory delete to fail");

    assert!(
        error
            .to_string()
            .contains("delete_file only supports regular files")
    );
    Ok(())
}

#[test]
fn register_builtin_tools_registers_multiple_tools() {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    assert!(registry.specs().len() >= 8);
    let spec = registry
        .spec_for("delete_file")
        .expect("delete_file should be registered");
    assert_eq!(spec.access, ToolAccess::Write);
    assert_eq!(spec.preview, ToolPreviewCapability::Required);
}

#[tokio::test]
async fn read_file_supports_offset_limit_and_truncation_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("big.txt"), "one\ntwo\nthree\nfour\n")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = ReadFileTool
        .execute(
            ctx,
            "read".to_owned(),
            json!({ "path": "big.txt", "offset": 1, "limit": 2 }),
        )
        .await?;

    assert!(result.content.starts_with("two\nthree"));
    assert!(result.content.contains("output truncated"));
    assert!(result.metadata.truncated);
    assert_eq!(result.metadata.returned_lines, Some(2));
    assert_eq!(result.metadata.total_lines, Some(4));
    assert_eq!(result.metadata.details["offset"], 1);
    assert_eq!(result.metadata.details["next_offset"], 3);
    Ok(())
}

#[tokio::test]
async fn list_glob_and_grep_report_limit_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for index in 0..5 {
        fs::write(temp.path().join(format!("file-{index}.txt")), "needle\n")?;
    }
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let list = ListTool
        .execute(ctx.clone(), "ls".to_owned(), json!({ "limit": 2 }))
        .await?;
    let glob = GlobTool
        .execute(
            ctx.clone(),
            "glob".to_owned(),
            json!({ "pattern": "*.txt", "limit": 2 }),
        )
        .await?;
    let grep = GrepTool
        .execute(
            ctx,
            "grep".to_owned(),
            json!({ "pattern": "needle", "limit": 2 }),
        )
        .await?;

    assert!(list.metadata.truncated);
    assert_eq!(list.metadata.returned_entries, Some(2));
    assert_eq!(list.metadata.total_entries, Some(5));
    assert!(glob.metadata.truncated);
    assert_eq!(glob.metadata.details["returned_paths"], 2);
    assert_eq!(glob.metadata.details["total_paths"], 5);
    assert!(grep.metadata.truncated);
    assert_eq!(grep.metadata.returned_matches, Some(2));
    assert_eq!(grep.metadata.total_matches, Some(5));
    Ok(())
}

#[tokio::test]
async fn bash_large_output_is_truncated_with_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = BashTool
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "yes x | head -n 70000" }),
        )
        .await?;

    assert!(result.metadata.truncated);
    assert!(result.content.contains("output truncated"));
    assert!(result.metadata.stdout_bytes.unwrap_or_default() > 64 * 1024);
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = ReadFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_file_reports_existing_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = WriteFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_file_reports_symlink_parent_escape_for_new_file_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let expected = outside.path().canonicalize()?.join("new.txt");
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects =
        WriteFileTool.permission_subjects(&ctx, &json!({ "path": "outside-dir/new.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert!(!outside.path().join("new.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn edit_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "hello old")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = EditFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "hello old");
    Ok(())
}

#[cfg(unix)]
#[test]
fn delete_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = DeleteFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[cfg(unix)]
#[test]
fn list_and_grep_report_external_symlink_roots_as_external_subjects() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let expected = outside.path().canonicalize()?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let list_subjects = ListTool.permission_subjects(&ctx, &json!({ "path": "outside-dir" }))?;
    let grep_subjects = GrepTool
        .permission_subjects(&ctx, &json!({ "path": "outside-dir", "pattern": "secret" }))?;

    assert_eq!(list_subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(grep_subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        list_subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(
        grep_subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn list_recursive_does_not_traverse_external_symlink_children() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    fs::write(workspace.path().join("visible.txt"), "visible")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = ListTool
        .execute(
            ctx,
            "list".to_owned(),
            json!({ "path": ".", "recursive": true }),
        )
        .await?;

    assert!(result.content.contains("visible.txt"));
    assert!(!result.content.contains("secret.txt"));
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

    let result = BashTool
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "sleep 2", "timeout_secs": 1 }),
        )
        .await?;

    let ToolResultStatus::Error(error) = result.status else {
        panic!("expected timeout to be surfaced as an error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Timeout);
    assert!(error.message.contains("bash command timed out"));
    Ok(())
}

#[tokio::test]
async fn bash_tool_non_zero_exit_returns_error_result() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = BashTool
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "printf 'bad output' >&2; exit 7" }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(result.metadata.exit_code, Some(7));
    assert!(result.content.contains("bad output"));
    Ok(())
}

#[test]
fn bash_permission_access_allows_only_simple_readonly_commands() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    for command in [
        "pwd",
        "ls src",
        "rg needle crates",
        "git status --short",
        "pwd && git status --short",
        "find . -name lib.rs",
        "command -v cargo",
        "rustc --version",
    ] {
        assert_eq!(
            BashTool.permission_access(&ctx, &json!({ "command": command }))?,
            ToolAccess::Read,
            "{command} should be read-only"
        );
    }

    for command in [
        "echo hi > out.txt",
        "echo $HOME",
        "pwd | wc -l",
        "ls *.rs",
        "(pwd)",
        "find . -exec echo {} \\;",
        "find . -delete",
        "git push",
        "python script.py",
        "cargo test",
    ] {
        assert_eq!(
            BashTool.permission_access(&ctx, &json!({ "command": command }))?,
            ToolAccess::Execute,
            "{command} should require execute approval"
        );
    }

    Ok(())
}

#[tokio::test]
async fn bash_permission_subjects_include_external_paths_and_redirections() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().canonicalize()?.join("input.txt");
    fs::write(&outside_file, "needle")?;
    let outside_output = outside.path().canonicalize()?.join("out.txt");
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = BashTool.permission_subjects(
        &ctx,
        &json!({ "command": format!("cat {} > {}", outside_file.display(), outside_output.display()) }),
    )?;

    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_file.as_path())
    }));
    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_output.as_path())
    }));
    Ok(())
}

#[tokio::test]
async fn bash_permission_subjects_resolve_cd_relative_paths_against_external_cwd() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_root = outside.path().canonicalize()?;
    let outside_child = outside_root.join("child.txt");
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let subjects = BashTool.permission_subjects(
        &ctx,
        &json!({ "command": format!("cd {} && ls child.txt", outside_root.display()) }),
    )?;

    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_root.as_path())
    }));
    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_child.as_path())
    }));
    Ok(())
}

#[tokio::test]
async fn grep_skips_non_utf8_files_without_panicking() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("valid.txt"), "needle\n")?;
    fs::write(temp.path().join("binary.bin"), [0xff_u8, 0xfe, 0xfd])?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = GrepTool
        .execute(ctx, "grep".to_owned(), json!({ "pattern": "needle" }))
        .await?;

    assert!(!result.is_error());
    assert!(result.content.contains("valid.txt"));
    assert!(!result.content.contains("binary.bin"));
    Ok(())
}

#[tokio::test]
async fn write_file_execute_creates_missing_parent_directories() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "nested/deep/note.txt", "content": "hello" }),
        )
        .await?;

    assert_eq!(
        fs::read_to_string(temp.path().join("nested/deep/note.txt"))?,
        "hello"
    );
    assert_eq!(result.metadata.changed_files, vec!["nested/deep/note.txt"]);
    Ok(())
}

#[tokio::test]
async fn edit_file_errors_for_missing_and_ambiguous_old_text() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    fs::write(temp.path().join("note.txt"), "repeat old repeat old")?;

    let missing = EditFileTool
        .execute(
            ctx.clone(),
            "edit-missing".to_owned(),
            json!({ "path": "note.txt", "old_text": "absent", "new_text": "new" }),
        )
        .await
        .expect_err("missing old_text should fail");
    assert!(missing.to_string().contains("old_text not found"));

    let ambiguous = EditFileTool
        .execute(
            ctx,
            "edit-ambiguous".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous old_text should fail");
    assert!(ambiguous.to_string().contains("old_text is ambiguous"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn delete_file_rejects_symlink_target() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("linked.txt"))?;
    let ctx = ToolContext {
        workspace_root: workspace.path().to_path_buf(),
        timeout_secs: 5,
    };

    let error = DeleteFileTool
        .execute(
            ctx,
            "delete-link".to_owned(),
            json!({ "path": "linked.txt" }),
        )
        .await
        .expect_err("symlink deletes should fail");

    assert!(error.to_string().contains("outside workspace"));
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[test]
fn builtin_path_and_truncation_helpers_preserve_boundaries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let subject = super::tool_path_subject(temp.path(), ".")?;
    assert_eq!(subject.scope, ToolSubjectScope::Workspace);
    assert_eq!(subject.normalized, ".");

    let repeated = "é".repeat(80);
    let truncated = super::limit_text_head_tail(&repeated, 32);
    assert!(truncated.truncated);
    assert!(truncated.content.contains("output truncated"));
    assert!(std::str::from_utf8(truncated.content.as_bytes()).is_ok());
    Ok(())
}
