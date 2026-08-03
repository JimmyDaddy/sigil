use super::*;

#[test]
fn parser_projects_branch_tracking_and_change_counts() {
    let status = parse_workspace_git_status(concat!(
        "## feature/tui-status...origin/feature/tui-status [ahead 2, behind 1]\n",
        "M  staged.rs\n",
        " M unstaged.rs\n",
        "MM both.rs\n",
        "?? new.rs\n",
        "UU conflicted.rs\n",
    ))
    .expect("git status");

    assert_eq!(status.branch, "feature/tui-status");
    assert_eq!(
        status.upstream.as_deref(),
        Some("origin/feature/tui-status")
    );
    assert_eq!(status.ahead, 2);
    assert_eq!(status.behind, 1);
    assert_eq!(status.changed_entries, 5);
    assert_eq!(status.staged_entries, 2);
    assert_eq!(status.unstaged_entries, 2);
    assert_eq!(status.untracked_entries, 1);
    assert_eq!(status.conflicted_entries, 1);
    assert_eq!(status.change_label(), "5 changes");
}

#[test]
fn parser_handles_clean_unborn_and_detached_worktrees() {
    let clean = parse_workspace_git_status("## No commits yet on main\n").expect("clean status");
    assert_eq!(clean.compact_label(), "main · clean");

    let detached =
        parse_workspace_git_status("## HEAD (no branch)\n?? note.txt\n").expect("detached status");
    assert_eq!(detached.branch, "detached HEAD");
    assert_eq!(detached.compact_label(), "detached HEAD · 1 change");
}

#[test]
fn parser_rejects_non_porcelain_output() {
    assert!(parse_workspace_git_status("").is_none());
    assert!(parse_workspace_git_status("not a repository\n").is_none());
}
