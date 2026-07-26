use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-env-changed=SIGIL_BUILD_GIT_HASH");
    println!("cargo:rerun-if-env-changed=SIGIL_BUILD_TARGET");
    println!("cargo:rerun-if-env-changed=SIGIL_BUILD_PROFILE");

    track_git_head();

    let git_hash = env::var("SIGIL_BUILD_GIT_HASH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_head_hash)
        .unwrap_or_else(|| "unknown".to_owned());
    let target = env::var("SIGIL_BUILD_TARGET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("TARGET").ok())
        .unwrap_or_else(|| "unknown".to_owned());
    let profile = env::var("SIGIL_BUILD_PROFILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("PROFILE").ok())
        .unwrap_or_else(|| "unknown".to_owned());

    rustc_env("SIGIL_BUILD_GIT_HASH", &git_hash);
    rustc_env("SIGIL_BUILD_TARGET", &target);
    rustc_env("SIGIL_BUILD_PROFILE", &profile);
}

fn rustc_env(name: &str, value: &str) {
    let sanitized = value.replace(['\n', '\r'], "");
    println!("cargo:rustc-env={name}={sanitized}");
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
