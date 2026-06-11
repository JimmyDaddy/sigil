use std::{fs, path::Path};

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

#[test]
fn builtin_argument_helpers_validate_types_and_sizes() {
    let missing = super::required_string(&json!({}), "path").expect_err("path should be required");
    assert!(missing.to_string().contains("missing string field path"));

    let wrong_type =
        super::required_string(&json!({ "path": 7 }), "path").expect_err("path should be string");
    assert!(wrong_type.to_string().contains("missing string field path"));

    let invalid_limit = super::optional_usize(&json!({ "limit": "many" }), "limit")
        .expect_err("limit should be numeric");
    assert!(
        invalid_limit
            .to_string()
            .contains("limit must be a positive integer")
    );
    assert_eq!(
        super::optional_string(&json!({ "path": "src" }), "path"),
        Some("src")
    );
    assert_eq!(
        super::optional_usize(&json!({ "limit": 3 }), "limit").expect("limit"),
        Some(3)
    );
}

#[tokio::test]
async fn tool_permission_subjects_validate_required_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    for (tool_name, result) in [
        (
            "read_file",
            ReadFileTool.permission_subjects(&ctx, &json!({})),
        ),
        (
            "write_file",
            WriteFileTool.permission_subjects(&ctx, &json!({ "content": "hello" })),
        ),
        (
            "edit_file",
            EditFileTool.permission_subjects(&ctx, &json!({ "old_text": "a", "new_text": "b" })),
        ),
        (
            "delete_file",
            DeleteFileTool.permission_subjects(&ctx, &json!({})),
        ),
    ] {
        let error = result.expect_err(tool_name);
        assert!(
            error.to_string().contains("missing string field path"),
            "{tool_name} should require a path"
        );
    }

    Ok(())
}

#[tokio::test]
async fn edit_file_preview_surfaces_missing_and_ambiguous_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.txt"), "old one old two")?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let missing = EditFileTool
        .preview(
            ctx.clone(),
            json!({ "path": "note.txt", "old_text": "absent", "new_text": "new" }),
        )
        .await
        .expect_err("missing old_text should fail preview");
    assert!(missing.to_string().contains("old_text not found"));

    let ambiguous = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous old_text should fail preview");
    assert!(ambiguous.to_string().contains("old_text is ambiguous"));
    Ok(())
}

#[tokio::test]
async fn read_list_glob_grep_and_bash_surface_input_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let read_error = ReadFileTool
        .execute(
            ctx.clone(),
            "read".to_owned(),
            json!({ "path": "missing.txt", "limit": "lots" }),
        )
        .await
        .expect_err("invalid read limit should fail");
    assert!(
        read_error
            .to_string()
            .contains("limit must be a positive integer")
    );

    let list_error = ListTool
        .execute(
            ctx.clone(),
            "ls".to_owned(),
            json!({ "path": "missing-dir" }),
        )
        .await
        .expect_err("missing list path should fail");
    assert!(!list_error.to_string().is_empty());

    let glob_error = GlobTool
        .execute(
            ctx.clone(),
            "glob".to_owned(),
            json!({ "pattern": "[", "limit": 5 }),
        )
        .await
        .expect_err("invalid glob should fail");
    assert!(!glob_error.to_string().is_empty());

    let grep_error = GrepTool
        .execute(ctx.clone(), "grep".to_owned(), json!({ "pattern": "[" }))
        .await
        .expect_err("invalid regex should fail");
    assert!(!grep_error.to_string().is_empty());

    let bash_error = BashTool
        .execute(ctx, "bash".to_owned(), json!({}))
        .await
        .expect_err("missing command should fail");
    assert!(
        bash_error
            .to_string()
            .contains("missing string field command")
    );
    Ok(())
}

#[test]
fn path_and_shell_helpers_cover_workspace_external_and_unknown_cases() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("outside.txt");
    fs::write(&outside_file, "outside")?;

    let workspace_subject = super::tool_path_subject(workspace.path(), "new/missing.txt")?;
    assert_eq!(workspace_subject.scope, ToolSubjectScope::Workspace);
    assert_eq!(workspace_subject.normalized, "new/missing.txt");

    let external_subject =
        super::tool_path_subject(workspace.path(), outside_file.to_string_lossy().as_ref())?;
    let expected_external_file = outside_file.canonicalize()?;
    assert_eq!(external_subject.scope, ToolSubjectScope::External);
    assert_eq!(
        external_subject.canonical_path.as_deref(),
        Some(expected_external_file.as_path())
    );

    assert_eq!(
        super::command_permission_subject("  git   status   --short  "),
        "git status --short"
    );
    let long_subject = super::command_permission_subject(&"x ".repeat(100));
    assert!(long_subject.ends_with("..."));
    assert!(super::bash_command_is_safe_readonly(
        "git branch --show-current"
    ));
    assert!(!super::bash_command_is_safe_readonly("git branch -D main"));
    assert!(!super::bash_command_is_safe_readonly("command"));
    assert!(!super::bash_command_is_safe_readonly(""));
    Ok(())
}

#[test]
fn diff_and_text_limit_helpers_handle_noop_and_head_limits() {
    let diff = super::render_unified_diff("same\n", "same\n", "current", "proposed");
    assert_eq!(diff, "No textual changes detected.");

    let limited = super::limit_text_head("one\ntwo\nthree\n", 8, 2);
    assert!(limited.truncated);
    assert_eq!(limited.returned_lines, 2);
    assert!(limited.content.contains("output truncated"));

    let unchanged = super::limit_text_head_tail("short", 128);
    assert!(!unchanged.truncated);
    assert_eq!(unchanged.content, "short");
    assert_eq!(unchanged.omitted_bytes, 0);
}

#[tokio::test]
async fn write_file_execute_creates_parent_dirs_and_reports_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };

    let result = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "nested/dir/note.txt", "content": "hello" }),
        )
        .await?;

    assert_eq!(
        fs::read_to_string(temp.path().join("nested/dir/note.txt"))?,
        "hello"
    );
    assert_eq!(result.metadata.changed_files, vec!["nested/dir/note.txt"]);
    assert_eq!(result.metadata.bytes, Some(5));
    Ok(())
}

#[tokio::test]
async fn edit_file_execute_and_preview_reject_missing_and_ambiguous_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 5,
    };
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old old\n")?;

    let ambiguous = EditFileTool
        .execute(
            ctx.clone(),
            "edit".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous replacements should fail");
    assert!(ambiguous.to_string().contains("ambiguous"));

    let missing = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "missing", "new_text": "new" }),
        )
        .await
        .expect_err("missing replacements should fail");
    assert!(missing.to_string().contains("not found"));
    Ok(())
}

#[test]
fn builtin_text_limit_and_path_helpers_cover_multibyte_edges() -> Result<()> {
    let limited = super::limit_text_head("one\ntwo\nthree", 7, 5);
    assert!(limited.truncated);
    assert!(limited.content.contains("output truncated"));

    let tail = super::limit_text_head_tail("abcdef", 5);
    assert!(tail.truncated);
    assert!(tail.content.contains("omitted"));
    assert!(tail.content.contains('\n'));

    let long_line = "x".repeat(super::MAX_MODEL_LINE_CHARS + 1);
    let truncated = super::truncate_line_for_model(&long_line);
    assert!(truncated.ends_with("[sigil: line truncated]"));

    let mut notice_only = String::new();
    super::append_truncation_notice(&mut notice_only);
    assert!(notice_only.starts_with("[sigil: output truncated"));

    let value = "a中b";
    assert_eq!(&value[..super::floor_char_boundary(value, 2)], "a");
    assert_eq!(&value[super::ceil_char_boundary(value, 2)..], "b");

    assert_eq!(
        super::lexically_normalize_path(Path::new("./notes/../draft.txt"))?,
        Path::new("draft.txt")
    );
    assert_eq!(
        super::lexically_normalize_path(Path::new("notes/../../draft.txt"))?,
        Path::new("../draft.txt")
    );

    let workspace = tempfile::tempdir()?;
    let resolved = super::resolve_existing_prefix(&workspace.path().join("missing/child.txt"))?;
    assert_eq!(
        resolved,
        workspace.path().canonicalize()?.join("missing/child.txt")
    );

    let missing_root = workspace.path().join("does-not-exist");
    assert!(
        super::canonical_workspace_root(&missing_root)
            .expect_err("missing workspaces should fail")
            .to_string()
            .contains("failed to resolve workspace root")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn delete_file_and_path_resolution_helpers_cover_external_and_symlink_paths() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let workspace_file = workspace.path().join("note.txt");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&workspace_file, "hello")?;
    fs::write(&outside_file, "secret")?;

    let target = super::resolve_delete_file_target(
        workspace.path(),
        workspace_file.to_str().expect("utf8 path"),
    )?;
    assert_eq!(target.path, workspace_file);
    assert_eq!(target.display_path, target.path.display().to_string());

    let outside_error = super::resolve_delete_file_target(
        workspace.path(),
        outside_file.to_str().expect("utf8 path"),
    )
    .expect_err("external delete targets should be rejected");
    assert!(outside_error.to_string().contains("outside workspace"));

    symlink(&outside_file, workspace.path().join("link.txt"))?;
    let symlink_error =
        super::validate_delete_file_target(&workspace.path().join("link.txt"), "link.txt")
            .expect_err("symlink delete targets should be rejected");
    assert!(
        symlink_error
            .to_string()
            .contains("does not support symlink")
    );
    Ok(())
}

#[test]
fn bash_and_shell_helper_functions_cover_parser_edges() -> Result<()> {
    assert!(!super::bash_command_is_safe_readonly(r#""""#));
    assert!(super::contains_unsupported_safe_shell_syntax("echo $HOME"));
    assert!(!super::bash_segment_is_safe_readonly(&[]));
    assert!(!super::bash_segment_is_safe_readonly(&[
        "cat".to_owned(),
        ">".to_owned(),
        "out.txt".to_owned(),
    ]));
    assert!(!super::git_segment_is_safe_readonly(&["git".to_owned()]));
    assert!(super::git_segment_is_safe_readonly(&[
        "git".to_owned(),
        "branch".to_owned(),
        "--list".to_owned(),
    ]));

    let tokens =
        super::tokenize_shell_subject_words(r#"echo "a b" foo\ bar && cat file || ls; pwd"#);
    assert_eq!(
        tokens,
        vec![
            "echo", "a b", "foo bar", "&&", "cat", "file", "||", "ls", ";", "pwd",
        ]
    );
    assert_eq!(super::redirection_target("1>out.txt"), Some("out.txt"));
    assert!(!super::is_path_argument("git", "--help"));
    assert!(super::is_path_argument("cat", "Cargo.toml"));
    assert!(!super::is_path_argument("echo", "Cargo.toml"));
    assert_eq!(
        super::render_unified_diff("same\n", "same\n", "a", "b"),
        "No textual changes detected."
    );

    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("note.txt"), "note")?;
    let workspace_root = workspace.path().canonicalize()?;

    let mut cwd = workspace_root.clone();
    let mut subjects = Vec::new();
    super::collect_bash_segment_subjects(&workspace_root, &mut cwd, &[], &mut subjects)?;
    assert!(subjects.is_empty());

    super::collect_bash_segment_subjects(
        &workspace_root,
        &mut cwd,
        &["cd".to_owned(), "-".to_owned()],
        &mut subjects,
    )?;
    assert_eq!(cwd, workspace_root);

    super::collect_bash_segment_subjects(
        &workspace_root,
        &mut cwd,
        &[
            "cat".to_owned(),
            "./note.txt".to_owned(),
            "1>out.txt".to_owned(),
            ">".to_owned(),
            "nested/out.txt".to_owned(),
        ],
        &mut subjects,
    )?;
    assert_eq!(subjects.len(), 3);
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "note.txt")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "out.txt")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "nested/out.txt")
    );
    Ok(())
}

#[test]
fn bash_path_subjects_and_tokenizer_cover_segmented_and_quoted_edges() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::create_dir(workspace.path().join("src"))?;
    fs::write(
        workspace.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )?;
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")?;
    let workspace_root = workspace.path().canonicalize()?;

    let tokens =
        super::tokenize_shell_subject_words(r#"echo "a\"b" && cat src/lib.rs || ls Cargo.toml"#);
    assert_eq!(
        tokens,
        vec![
            "echo",
            "a\"b",
            "&&",
            "cat",
            "src/lib.rs",
            "||",
            "ls",
            "Cargo.toml",
        ]
    );

    let subjects =
        super::bash_path_subjects(workspace.path(), "cd src && cat lib.rs || ls ../Cargo.toml")?;

    assert_eq!(subjects.len(), 3);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(workspace_root.join("src").as_path())
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "src/lib.rs")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "Cargo.toml")
    );
    Ok(())
}

#[test]
fn lexical_normalize_path_returns_dot_for_current_directory() -> Result<()> {
    assert_eq!(
        super::lexically_normalize_path(Path::new("."))?,
        Path::new(".")
    );
    Ok(())
}
