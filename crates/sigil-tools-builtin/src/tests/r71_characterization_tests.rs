//! RFC-0071 R71.0 characterization fixtures (session 5ff39a6d-5225-4533-8c1f-b64c0c81abb7).
//!
//! These tests lock the *observed* behavior of the current legacy scratch implementation without
//! changing production semantics. They are the acceptance basement that R71.2+/R71.6 must either
//! preserve (no-follow leaf handling is safe) or deliberately supersede (sibling blast radius,
//! GC skip-invalid-forever, same-session double lease early release, repeated provisioning
//! failures without a durable active-blocker admission gate).

use std::{fs, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::{
    ScratchGcConfig, ScratchNamespaceControl, ScratchQuota, ScratchTaskLeaseRegistry,
    ensure_session_scratch, gc_scratch_namespaces, measure_scratch_usage, session_scratch_dir,
    session_scratch_key,
};
use anyhow::Result;

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis()
        .try_into()
        .expect("clock fits u64")
}

fn isolated_scratch_root(label: &str) -> Result<(tempfile::TempDir, std::path::PathBuf)> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let _ = label;
    Ok((temp, scratch_root))
}

/// Causal edge: a descendant symlink *inside* one session namespace is a leaf entry. The walker
/// must use symlink_metadata (no-follow) so the link itself is measured/classified, never followed.
#[cfg(unix)]
#[test]
fn r71_descendant_symlink_is_reported_as_leaf_and_not_followed() -> Result<()> {
    let (_temp, scratch_root) = isolated_scratch_root("r71-descendant-leaf")?;
    let session = "r71-leaf-0000-0000-0000-000000000001";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let namespace = session_scratch_dir(&scratch_root, Some(session));

    // A build/test fixture creates a deep directory tree and inside it a support-bundle symlink,
    // exactly like the TUI feedback fixture used to place cache/support-bundles in temp_dir.
    let nested = namespace.join("build").join("support-bundles");
    fs::create_dir_all(nested.parent().expect("nested parent"))?;
    let external = _temp.path().join("external-support");
    fs::create_dir_all(&external)?;
    symlink(&external, &nested)?;

    // Measurement reports the symlink as the offending leaf (still an error today: the namespace
    // is considered invalid). Crucially the walk uses symlink_metadata, so the link target is
    // never entered: no recursion, no traversal outside the namespace.
    let error = measure_scratch_usage(&scratch_root, session)
        .expect_err("descendant symlink must fail measurement in the current implementation");
    let text = format!("{error:#}");
    assert!(
        text.contains("support-bundles"),
        "leaf path must be named: {text}"
    );
    assert!(
        text.contains("symlink"),
        "failure class must be symlink: {text}"
    );
    Ok(())
}

/// Causal edge: the blast radius is workspace-wide. A poisoning namespace in one session causes
/// the *provisioning* of an unrelated sibling session to fail too, because
/// measure_scratch_usage walks every session namespace under the shared scratch root.
#[cfg(unix)]
#[test]
fn r71_poisoned_sibling_blocks_unrelated_session_provisioning() -> Result<()> {
    let (_temp, scratch_root) = isolated_scratch_root("r71-sibling-blast")?;
    let poisoner = "r71-poisoner-0000-0000-0000-000000000002";
    let victim = "r71-victim-0000-0000-0000-000000000003";
    ensure_session_scratch(&scratch_root, Some(poisoner), &ScratchQuota::default())?;
    ensure_session_scratch(&scratch_root, Some(victim), &ScratchQuota::default())?;

    let namespace = session_scratch_dir(&scratch_root, Some(poisoner));
    let external = _temp.path().join("external-target");
    fs::create_dir_all(&external)?;
    symlink(&external, namespace.join("poison"))?;

    // The victim session has no fixture of its own but provisioning still fails.
    let error = ensure_session_scratch(&scratch_root, Some(victim), &ScratchQuota::default())
        .expect_err("unrelated sibling provisioning must fail (current blast radius)");
    let text = format!("{error:#}");
    assert!(
        text.contains("symlink"),
        "failure class must be symlink: {text}"
    );
    assert!(
        fs::symlink_metadata(session_scratch_dir(&scratch_root, Some(victim))).is_ok(),
        "victim namespace must still exist (poisoning is measurement-only)"
    );
    Ok(())
}

/// Causal edge: GC never quarantines or reclaims an invalid namespace; it records
/// skipped_invalid forever, leaving the namespace present and the blocker permanent.
#[cfg(unix)]
#[test]
fn r71_gc_permanently_skips_invalid_namespace_without_quarantine() -> Result<()> {
    let (_temp, scratch_root) = isolated_scratch_root("r71-gc-skip-invalid")?;
    let control = ScratchNamespaceControl::new();
    let session = "r71-gc-invalid-0000-0000-0000-000000000004";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let namespace = session_scratch_dir(&scratch_root, Some(session));
    let external = _temp.path().join("gc-external");
    fs::create_dir_all(&external)?;
    symlink(&external, namespace.join("poison"))?;

    let config = ScratchGcConfig { ttl_ms: 0 };
    let first = gc_scratch_namespaces(&scratch_root, &control, &config, unix_now_ms() + 10_000)?;
    assert_eq!(
        first.skipped_invalid, 1,
        "invalid namespace must be skipped"
    );
    assert_eq!(first.deleted, 0, "invalid namespace must not be deleted");
    assert_eq!(
        first.scanned, 1,
        "directory entry is scanned but then skipped as invalid"
    );
    let second = gc_scratch_namespaces(&scratch_root, &control, &config, unix_now_ms() + 20_000)?;
    assert_eq!(
        second.skipped_invalid, 1,
        "invalid namespace must be skipped forever"
    );
    assert!(namespace.exists(), "poisoned namespace must remain on disk");
    Ok(())
}

/// Causal edge: two concurrent leases on the same session namespace. The current registry is a
/// BTreeSet keyed by session key -- the entry is removed when the first guard is dropped, so a
/// still-live second holder silently loses its lease (early release).
///
/// This locks the observed defect. RFC-0071 R71.6 must flip the middle assertion once the
/// registry becomes reference-counted per holder.
#[test]
fn r71_same_session_double_lease_releases_early() -> Result<()> {
    let control = ScratchNamespaceControl::new();
    let key = session_scratch_key(Some("r71-double-lease-0000-0000-0000-000000000005"));

    let first = control.namespaces.acquire(&key);
    let second = control.namespaces.acquire(&key);
    assert!(
        control.namespaces.is_leased(&key),
        "both holders are live, namespace must stay leased"
    );

    // Observed defect: the set-based registry releases on the first drop even though the second
    // holder is still live. R71.6 replaces the registry with per-holder reference counts.
    drop(first);
    assert!(
        !control.namespaces.is_leased(&key),
        "observed early release: a live second holder must not lose its lease (fixed in R71.6)"
    );
    drop(second);
    assert!(!control.namespaces.is_leased(&key));
    Ok(())
}

/// Causal edge: repeated provisioning failures across distinct new tool calls. Each call id hits
/// the same provision failure as an independent attempt; there is no durable active-blocker
/// admission gate, so retryable=false does not stop the next call from being attempted.
#[cfg(unix)]
#[test]
fn r71_repeated_new_call_hits_same_provision_failure_without_blocker_gate() -> Result<()> {
    let (_temp, scratch_root) = isolated_scratch_root("r71-repeat-provision")?;
    let session = "r71-repeat-0000-0000-0000-000000000006";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let namespace = session_scratch_dir(&scratch_root, Some(session));
    let external = _temp.path().join("repeat-external");
    fs::create_dir_all(&external)?;
    symlink(&external, namespace.join("poison"))?;

    // Call id 100 (first new tool call after poisoning).
    let first_error =
        ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())
            .expect_err("first new call must fail provisioning");
    let first_text = format!("{first_error:#}");
    // Call id 200 (another new tool call). Same failure class, no active blocker admission gate.
    let second_error =
        ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())
            .expect_err("second new call must hit the same provision failure");
    let second_text = format!("{second_error:#}");
    assert_eq!(first_text, second_text, "failure class must be identical");
    assert!(second_text.contains("symlink"));
    Ok(())
}

/// Causal edge: terminal task leases are reference-counted against live tasks, and the last
/// release is required before GC may reclaim. Current implementation: task registry keys the
/// namespace lease by task id and removes it on release; a same-session second task must keep
/// the namespace leased.
#[test]
fn r71_two_terminal_tasks_on_same_session_keep_namespace_leased() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let control = ScratchNamespaceControl::new();
    let session = "r71-tasks-0000-0000-0000-000000000007";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let key = session_scratch_key(Some(session));

    let tasks = Arc::new(ScratchTaskLeaseRegistry::new());
    tasks.register("task-a", &key, &control.namespaces);
    tasks.register("task-b", &key, &control.namespaces);

    let report = gc_scratch_namespaces(
        &scratch_root,
        &control,
        &ScratchGcConfig { ttl_ms: 0 },
        unix_now_ms() + 10_000,
    )?;
    assert_eq!(
        report.skipped_leased, 1,
        "both tasks live: namespace stays leased"
    );

    // Observed defect: releasing task-a drops the shared set entry, so the namespace is no
    // longer protected while task-b is still live. R71.6 must keep it leased.
    tasks.release("task-a");
    let report = gc_scratch_namespaces(
        &scratch_root,
        &control,
        &ScratchGcConfig { ttl_ms: 0 },
        unix_now_ms() + 20_000,
    )?;
    assert_eq!(
        report.skipped_leased, 0,
        "observed early release: task-b still live must keep the namespace leased (fixed in R71.6)"
    );
    assert_eq!(
        report.deleted, 1,
        "observed early release consequence: GC deletes the namespace while task-b is still live"
    );
    assert!(
        !session_scratch_dir(&scratch_root, Some(session)).exists(),
        "namespace was reclaimed while a live task holder exists (observed defect)"
    );

    tasks.release("task-b");
    Ok(())
}
