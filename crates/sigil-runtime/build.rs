use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-env-changed=SIGIL_RUNTIME_BUILD_GIT_HASH");
    track_git_head();
    let git_hash = env::var("SIGIL_RUNTIME_BUILD_GIT_HASH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_head_hash)
        .unwrap_or_else(|| "unknown".to_owned());
    let sanitized = git_hash.replace(['\n', '\r'], "");
    println!("cargo:rustc-env=SIGIL_RUNTIME_BUILD_GIT_HASH={sanitized}");
}

fn git_head_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let trimmed = hash.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn track_git_head() {
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(value) => PathBuf::from(value),
        Err(_) => return,
    };
    let git_dir = manifest_dir.join("../../.git");
    let git_head = git_dir.join("HEAD");
    if !git_head.exists() {
        return;
    }
    println!("cargo:rerun-if-changed={}", git_head.display());

    let Ok(head) = fs::read_to_string(&git_head) else {
        return;
    };
    let Some(ref_path) = head.strip_prefix("ref:").map(str::trim) else {
        return;
    };
    let ref_path = Path::new(ref_path);
    if ref_path.is_absolute() {
        return;
    }
    let git_ref = git_dir.join(ref_path);
    if git_ref.exists() {
        println!("cargo:rerun-if-changed={}", git_ref.display());
    }
}
