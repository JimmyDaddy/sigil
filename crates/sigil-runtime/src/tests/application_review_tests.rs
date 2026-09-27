use std::{fs, path::PathBuf};

use sigil_application::SafeText;
use sigil_kernel::{
    ControlEntry, ControlledCheckpoint, ControlledCheckpointRestoreRequest, JsonlSessionStore,
    ModelMessage, MutationEventRecorder, SessionLogEntry, ToolDiffBudget, ToolPreview,
    ToolPreviewFile, ToolPreviewSnapshot, write_file_with_mutation,
};

use super::*;

struct ReviewFixture {
    temp: tempfile::TempDir,
    workspace: PathBuf,
    session_path: PathBuf,
    scope: String,
    checkpoint: ControlledCheckpoint,
    recorder: MutationEventRecorder,
}

fn recorded_changes() -> Result<ReviewFixture> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "create two notes",
    )))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store.clone(), temp.path().join("artifacts"));
    for (call, path, contents) in [
        ("call-a", "a.txt", "first\nsecond\n"),
        ("call-b", "b.txt", "other\n"),
    ] {
        let diff = format!(
            "--- /dev/null\n+++ {path}\n@@ -0,0 +1,{} @@\n{}",
            contents.lines().count(),
            contents
                .lines()
                .map(|line| format!("+{line}\n"))
                .collect::<String>()
        );
        let preview = ToolPreviewSnapshot::from_preview(
            call,
            "write_file",
            &ToolPreview {
                title: "Create note".to_owned(),
                summary: "Recorded creation".to_owned(),
                body: String::new(),
                changed_files: vec![path.to_owned()],
                file_diffs: vec![ToolPreviewFile {
                    path: path.to_owned(),
                    diff,
                }],
            },
            ToolDiffBudget::default(),
            None,
        );
        store.append(&SessionLogEntry::Control(
            ControlEntry::ToolPreviewCaptured(preview),
        ))?;
        write_file_with_mutation(
            Some(&recorder),
            &workspace,
            call,
            path,
            workspace.join(path),
            contents.as_bytes(),
        )?;
    }
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let scope = records[0].session_id().to_owned();
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .context("checkpoint")?
        .clone();
    Ok(ReviewFixture {
        temp,
        workspace,
        session_path,
        scope,
        checkpoint,
        recorder,
    })
}

fn review(fixture: &ReviewFixture) -> Result<ApplicationCheckpointReview> {
    application_checkpoint_review(
        &fixture.session_path,
        &fixture.scope,
        &fixture.workspace,
        &fixture.checkpoint.checkpoint_id,
        &fixture.checkpoint.checkpoint_digest,
    )
}

fn annotation(review: &ApplicationCheckpointReview) -> Result<ReviewAnnotation> {
    let diff = review
        .diffs
        .iter()
        .find(|diff| diff.path == "a.txt")
        .context("a diff")?;
    Ok(ReviewAnnotation {
        checkpoint_id: review.checkpoint_id.clone(),
        checkpoint_digest: review.checkpoint_digest.clone(),
        source_call_id: diff.source_call_id.clone(),
        diff_digest: diff.diff_digest.clone(),
        path: diff.path.clone(),
        side: ReviewDiffSide::New,
        start_line: 1,
        end_line: 2,
        comment: SafeText::new("Please clarify these two lines")?,
    })
}

#[test]
fn review_annotations_ablate_restore_readiness_and_preserve_stale_source_without_writes()
-> Result<()> {
    let fixture = recorded_changes()?;
    let original = review(&fixture)?;
    assert_eq!(original.diffs.len(), 2);
    assert!(
        original
            .diffs
            .iter()
            .all(|diff| diff.file_state == ReviewFileState::Current)
    );
    let comment = annotation(&original)?;
    fs::write(fixture.workspace.join("b.txt"), "unrelated external edit\n")?;
    let records = JsonlSessionStore::read_event_records(&fixture.session_path)?;
    // Candidate-gate control: a complete restore preflight refuses this same checkpoint.
    // The paired source patch applies that guard to comment submission; it is not an old UI feature.
    let restore = sigil_kernel::preview_controlled_checkpoint_restore(
        &fixture.recorder,
        &records,
        &fixture.workspace,
        &ControlledCheckpointRestoreRequest {
            checkpoint_id: comment.checkpoint_id.clone(),
            checkpoint_digest: comment.checkpoint_digest.clone(),
        },
    )?;
    assert!(!restore.ready);
    let context = materialize_review_annotations(
        &fixture.session_path,
        &fixture.scope,
        &fixture.workspace,
        std::slice::from_ref(&comment),
    )?;
    assert!(context.contains("matches this recorded change content"));
    fs::write(
        fixture.workspace.join("a.txt"),
        "new content after the recorded change\n",
    )?;
    let log_before = fs::read(&fixture.session_path)?;
    let a_before = fs::read(fixture.workspace.join("a.txt"))?;
    let b_before = fs::read(fixture.workspace.join("b.txt"))?;
    assert_eq!(
        review(&fixture)?.diffs[0].file_state,
        ReviewFileState::Changed
    );
    let context = materialize_review_annotations(
        &fixture.session_path,
        &fixture.scope,
        &fixture.workspace,
        &[comment],
    )?;
    assert!(context.contains("outdated:"));
    assert!(context.contains("1: +first\n2: +second"));
    assert!(context.contains("no file-write authority"));
    assert_eq!(fs::read(&fixture.session_path)?, log_before);
    assert_eq!(fs::read(fixture.workspace.join("a.txt"))?, a_before);
    assert_eq!(fs::read(fixture.workspace.join("b.txt"))?, b_before);
    Ok(())
}

#[test]
fn review_annotations_reject_forged_reference_but_allow_unknown_current_file() -> Result<()> {
    let fixture = recorded_changes()?;
    let comment = annotation(&review(&fixture)?)?;
    let mut wrong = comment.clone();
    for field in ["checkpoint", "diff", "call", "path", "line", "side"] {
        match field {
            "checkpoint" => wrong.checkpoint_digest = "wrong".to_owned(),
            "diff" => wrong.diff_digest = "wrong".to_owned(),
            "call" => wrong.source_call_id = "wrong".to_owned(),
            "path" => wrong.path = "../outside.txt".to_owned(),
            "line" => wrong.end_line = 3,
            "side" => wrong.side = ReviewDiffSide::Old,
            _ => unreachable!(),
        }
        assert!(
            materialize_review_annotations(
                &fixture.session_path,
                &fixture.scope,
                &fixture.workspace,
                &[wrong]
            )
            .is_err(),
            "{field}"
        );
        wrong = comment.clone();
    }
    assert!(
        application_checkpoint_review(
            &fixture.session_path,
            "foreign",
            &fixture.workspace,
            &comment.checkpoint_id,
            &comment.checkpoint_digest
        )
        .is_err()
    );
    let foreign_workspace = fixture.temp.path().join("foreign");
    fs::create_dir(&foreign_workspace)?;
    assert!(
        application_checkpoint_review(
            &fixture.session_path,
            &fixture.scope,
            &foreign_workspace,
            &comment.checkpoint_id,
            &comment.checkpoint_digest
        )
        .is_err()
    );
    fs::remove_file(fixture.workspace.join("a.txt"))?;
    let context = materialize_review_annotations(
        &fixture.session_path,
        &fixture.scope,
        &fixture.workspace,
        &[comment],
    )?;
    assert!(context.contains("current file content unknown"));
    assert!(!fixture.workspace.join("a.txt").exists());
    Ok(())
}

#[test]
fn review_diff_numbering_keeps_headers_outside_declared_hunk_ranges() {
    let lines = numbered_diff_lines(
        "--- a/x\n+++ b/x\n@@ -3,2 +5,2 @@\n keep\n-old\n+new\n+not-in-hunk\n@@ -8 +9 @@\n-old2\n+new2",
    );
    assert_eq!((lines[3].old_line, lines[3].new_line), (Some(3), Some(5)));
    assert_eq!((lines[4].old_line, lines[4].new_line), (Some(4), None));
    assert_eq!((lines[5].old_line, lines[5].new_line), (None, Some(6)));
    assert_eq!(lines[6].new_line, None);
    assert_eq!((lines[8].old_line, lines[9].new_line), (Some(8), Some(9)));
}

#[test]
fn review_file_state_uses_the_selected_call_version_not_the_last_checkpoint_hash() -> Result<()> {
    let mut fixture = recorded_changes()?;
    let store = JsonlSessionStore::new(&fixture.session_path)?;
    let preview = ToolPreviewSnapshot::from_preview(
        "call-later",
        "write_file",
        &ToolPreview {
            title: "Revise note".to_owned(),
            summary: "Second edit in the same turn".to_owned(),
            body: String::new(),
            changed_files: vec!["a.txt".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "a.txt".to_owned(),
                diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1 @@\n-first\n-second\n+third\n"
                    .to_owned(),
            }],
        },
        ToolDiffBudget::default(),
        None,
    );
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(preview),
    ))?;
    write_file_with_mutation(
        Some(&fixture.recorder),
        &fixture.workspace,
        "call-later",
        "a.txt",
        fixture.workspace.join("a.txt"),
        b"third\n",
    )?;
    fixture.checkpoint = ControlledCheckpointProjection::from_records(
        &JsonlSessionStore::read_event_records(&fixture.session_path)?,
    )?
    .latest()
    .context("updated checkpoint")?
    .clone();
    let view = review(&fixture)?;
    assert_eq!(
        view.diffs
            .iter()
            .find(|diff| diff.source_call_id == "call-a")
            .context("old version")?
            .file_state,
        ReviewFileState::Changed
    );
    assert_eq!(
        view.diffs
            .iter()
            .find(|diff| diff.source_call_id == "call-later")
            .context("current version")?
            .file_state,
        ReviewFileState::Current
    );
    Ok(())
}

#[test]
fn queued_review_context_keeps_exact_prompt_identity_across_current_file_drift() -> Result<()> {
    let fixture = recorded_changes()?;
    let comment = annotation(&review(&fixture)?)?;
    let queued = || {
        materialize_queued_review_annotations(
            &fixture.session_path,
            &fixture.scope,
            &fixture.workspace,
            std::slice::from_ref(&comment),
        )
    };
    let first = queued()?;
    assert!(first.contains("1: +first\n2: +second"));
    assert!(first.contains("unknown at execution"));
    let first_identity =
        sigil_kernel::project_conversation_prompt_for_persistence(&first).prompt_hash;
    fs::write(
        fixture.workspace.join("a.txt"),
        "changed after source selection\n",
    )?;
    assert_eq!(
        review(&fixture)?.diffs[0].file_state,
        ReviewFileState::Changed
    );
    assert_eq!(queued()?, first);
    fs::remove_file(fixture.workspace.join("a.txt"))?;
    let retry = queued()?;
    assert_eq!(retry, first);
    assert_eq!(
        sigil_kernel::project_conversation_prompt_for_persistence(&retry).prompt_hash,
        first_identity
    );
    Ok(())
}
