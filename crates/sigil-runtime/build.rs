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
    let Some(git_head) = git_path(&manifest_dir, Path::new("HEAD")) else {
        return;
    };
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
    if let Some(git_ref) = git_path(&manifest_dir, ref_path) {
        println!("cargo:rerun-if-changed={}", git_ref.display());
    }
    if let Some(packed_refs) = git_path(&manifest_dir, Path::new("packed-refs")) {
        println!("cargo:rerun-if-changed={}", packed_refs.display());
    }
}

fn git_path(manifest_dir: &Path, path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "--git-path"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let resolved = PathBuf::from(raw.trim());
    if resolved.as_os_str().is_empty() {
        return None;
    }
    Some(if resolved.is_absolute() {
        resolved
    } else {
        manifest_dir.join(resolved)
    })
}
