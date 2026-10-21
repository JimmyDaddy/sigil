use std::collections::BTreeSet;

use super::*;

#[tokio::test]
async fn recent_store_persists_private_paths_but_projects_only_safe_summary() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace should create");
    let path = temp.path().join("state/recent.json");
    let mut store = RecentWorkspaceStore::new(path.clone());
    store
        .upsert(
            "workspace-safe".to_owned(),
            "Workspace".to_owned(),
            &workspace,
        )
        .await
        .expect("recent workspace should persist");

    let mut reopened = RecentWorkspaceStore::new(path);
    let summaries = reopened
        .list(&BTreeSet::from(["workspace-safe".to_owned()]))
        .await
        .expect("recent workspace should load");
    assert_eq!(
        summaries,
        vec![RecentWorkspaceSummary {
            id: "workspace-safe".to_owned(),
            display_name: "Workspace".to_owned(),
            location_label: sigil_desktop::workspace_location_label(&workspace),
            is_open: true,
        }]
    );
    let projection = serde_json::to_string(&summaries).expect("summary should serialize");
    assert!(!projection.contains(workspace.to_string_lossy().as_ref()));
    assert_eq!(
        reopened
            .resolve("workspace-safe")
            .await
            .expect("record should resolve")
            .0,
        workspace
            .canonicalize()
            .expect("workspace should canonicalize")
    );
}

#[tokio::test]
async fn recent_store_rejects_oversized_or_unknown_records_without_path_errors() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let path = temp.path().join("recent.json");
    std::fs::write(&path, vec![b'x'; MAX_RECENT_FILE_BYTES as usize + 1])
        .expect("oversized fixture should write");
    let mut store = RecentWorkspaceStore::new(path);
    assert!(matches!(
        store.list(&BTreeSet::new()).await,
        Err(RecentWorkspaceStoreError::InvalidFile)
    ));

    let mut empty = RecentWorkspaceStore::new(temp.path().join("missing.json"));
    let error = empty
        .resolve("unknown")
        .await
        .expect_err("unknown recent workspace should fail");
    assert_eq!(error.to_string(), "recent workspace is unknown");
    assert!(
        !error
            .to_string()
            .contains(temp.path().to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn recent_store_disambiguates_same_names_without_projecting_paths() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let first = temp.path().join("original/shared/desktop-workspace");
    let second = temp.path().join("polish/shared/desktop-workspace");
    std::fs::create_dir_all(&first).expect("first workspace should create");
    std::fs::create_dir_all(&second).expect("second workspace should create");
    let mut store = RecentWorkspaceStore::new(temp.path().join("recent.json"));
    for (id, path) in [("original", &first), ("polish", &second)] {
        store
            .upsert(id.to_owned(), "desktop-workspace".to_owned(), path)
            .await
            .expect("workspace should persist");
    }
    let summaries = store
        .list(&BTreeSet::new())
        .await
        .expect("recent list should project");
    assert_eq!(summaries[0].display_name, summaries[1].display_name);
    assert_eq!(
        summaries[0].location_label.as_deref(),
        Some("shared · polish")
    );
    assert_eq!(
        summaries[1].location_label.as_deref(),
        Some("shared · original")
    );
    let projected = serde_json::to_string(&summaries).expect("summaries serialize");
    assert!(!projected.contains(temp.path().to_string_lossy().as_ref()));
    assert!(!projected.contains("workspaceRoot"));
    assert!(!projected.contains("workspace_root"));
}
