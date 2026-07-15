use std::{fs, path::Path};

use tempfile::tempdir;

use crate::model_eval::{load_model_eval_fixture, materialize_model_eval_fixture};

fn fixture_root(id: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dev/evals/model-fixtures")
        .join(id)
}

#[test]
fn committed_model_eval_fixtures_load_and_materialize() {
    for id in [
        "small-doc-edit",
        "small-code-edit",
        "stale-after-write",
        "workspace-trust",
        "sandbox-denial",
    ] {
        let fixture = load_model_eval_fixture(fixture_root(id)).expect("fixture should load");
        assert_eq!(fixture.manifest.id, id);
        let temp = tempdir().expect("temp dir");
        let destination = temp.path().join("workspace");
        let materialized =
            materialize_model_eval_fixture(&fixture, &destination).expect("materialize fixture");
        assert_eq!(materialized.fixture_id, id);
        assert!(materialized.tree_digest.starts_with("sha256:"));
        assert!(destination.join("Cargo.toml").is_file());
        assert!(!materialized.tool_scope.allows("bash"));
        assert!(!materialized.tool_scope.allows("websearch"));
    }
}

#[test]
fn model_eval_fixture_rejects_digest_drift() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    fs::write(
        temp.path().join("files/src/lib.rs"),
        "pub fn value() -> u32 { 9 }\n",
    )
    .expect("drift source");

    let error = load_model_eval_fixture(temp.path()).expect_err("digest drift must fail");
    assert!(error.to_string().contains("file sha256 mismatch"));
}

#[test]
fn model_eval_fixture_rejects_unknown_fields_and_tools() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let manifest_path = temp.path().join("fixture.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "allowed_tools = [\"read_file\", \"edit_file\"]",
            "allowed_tools = [\"read_file\", \"bash\"]\nunknown = true",
        ),
    )
    .expect("write manifest");

    let error = load_model_eval_fixture(temp.path()).expect_err("unknown field must fail");
    assert!(error.to_string().contains("failed to parse"));
}

#[cfg(unix)]
#[test]
fn model_eval_fixture_rejects_symlinked_sources() {
    use std::os::unix::fs::symlink;

    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let file = temp.path().join("files/src/lib.rs");
    fs::remove_file(&file).expect("remove copied source");
    symlink("../../prompt.txt", &file).expect("create symlink");

    let error = load_model_eval_fixture(temp.path()).expect_err("symlink must fail");
    assert!(error.to_string().contains("not a regular file"));
}

#[test]
fn model_eval_materializer_refuses_existing_destination() {
    let fixture = load_model_eval_fixture(fixture_root("small-doc-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let error = materialize_model_eval_fixture(&fixture, temp.path())
        .expect_err("existing destination must fail");
    assert!(error.to_string().contains("already exists"));
}

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            fs::create_dir_all(&target).expect("copy directory");
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}
