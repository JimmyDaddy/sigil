use std::{fs, sync::Arc, time::SystemTime};

#[cfg(unix)]
use std::{path::Path, time::Duration};

use anyhow::Result;
use sigil_kernel::private_path_permissions_are_restricted;

use crate::scratch_namespace::{
    ScratchGcConfig, ScratchNamespaceControl, ScratchQuota, ScratchQuotaExceededError,
    ScratchQuotaScope, ScratchTaskLeaseRegistry, ScratchUsage, delete_session_scratch_namespace,
    ensure_session_scratch, gc_scratch_namespaces, measure_scratch_usage, session_scratch_dir,
    session_scratch_key,
};

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis()
        .try_into()
        .expect("clock fits u64")
}

#[cfg(unix)]
fn set_modified_recursively(root: &Path, age_ms: u64) -> Result<()> {
    fn visit(path: &Path, age_ms: u64) -> Result<()> {
        let stamp = SystemTime::now() - Duration::from_millis(age_ms);
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path)?;
            if metadata.is_dir() {
                visit(&entry_path, age_ms)?;
            }
            std::fs::File::open(&entry_path)?.set_modified(stamp)?;
        }
        std::fs::File::open(path)?.set_modified(stamp)?;
        Ok(())
    }
    visit(root, age_ms)
}

#[test]
fn session_scratch_namespaces_are_isolated_per_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let quota = ScratchQuota::default();

    let session_a = "11111111-2222-3333-4444-555555555555";
    let session_b = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let provision_a = ensure_session_scratch(&scratch_root, Some(session_a), &quota)?;
    let provision_b = ensure_session_scratch(&scratch_root, Some(session_b), &quota)?;

    assert_ne!(provision_a.dir, provision_b.dir);
    assert_eq!(
        provision_a.dir,
        session_scratch_dir(&scratch_root, Some(session_a))
    );
    assert_eq!(
        provision_b.dir,
        session_scratch_dir(&scratch_root, Some(session_b))
    );
    assert_eq!(session_scratch_key(Some(session_a)), session_a);
    assert_eq!(session_scratch_key(Some(session_b)), session_b);

    fs::write(provision_a.dir.join("only-a.txt"), "a-content")?;
    fs::write(provision_b.dir.join("only-b.txt"), "b-content")?;

    let usage_a = measure_scratch_usage(&scratch_root, session_a)?;
    let usage_b = measure_scratch_usage(&scratch_root, session_b)?;
    assert_eq!(usage_a.session_bytes, "a-content".len() as u64);
    assert_eq!(usage_b.session_bytes, "b-content".len() as u64);
    assert_eq!(usage_a.session_entry_count, 1);
    assert_eq!(usage_b.session_entry_count, 1);
    assert_eq!(usage_a.workspace_bytes, usage_b.workspace_bytes);
    assert_eq!(usage_a.workspace_entry_count, 2);
    assert!(!provision_b.dir.join("only-a.txt").exists());
    assert!(!provision_a.dir.join("only-b.txt").exists());
    Ok(())
}

#[test]
fn resumed_session_reuses_the_same_namespace_and_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let quota = ScratchQuota::default();
    let session = "resume-me-0000-0000-0000-000000000001";

    let first = ensure_session_scratch(&scratch_root, Some(session), &quota)?;
    fs::write(first.dir.join("draft.txt"), "draft-v1")?;

    // A resumed session derives the same namespace and keeps its content (lifecycle: content
    // survives as long as the namespace is not reclaimed by TTL).
    let resumed = ensure_session_scratch(&scratch_root, Some(session), &quota)?;
    assert_eq!(resumed.dir, first.dir);
    assert_eq!(
        fs::read_to_string(resumed.dir.join("draft.txt"))?,
        "draft-v1"
    );
    Ok(())
}

#[test]
fn no_session_scope_uses_stable_fallback_namespace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let quota = ScratchQuota::default();
    let first = ensure_session_scratch(&scratch_root, None, &quota)?;
    let second = ensure_session_scratch(&scratch_root, None, &quota)?;
    assert_eq!(first.dir, second.dir);
    assert!(first.dir.ends_with("no-session"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_namespace_is_rejected() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside)?;
    fs::create_dir_all(scratch_root.join("sessions"))?;
    symlink(
        &outside,
        scratch_root.join("sessions").join("attacker-session"),
    )?;

    let error = ensure_session_scratch(
        &scratch_root,
        Some("attacker-session"),
        &ScratchQuota::default(),
    )
    .expect_err("symlinked namespace must be rejected");
    assert!(format!("{error:#}").contains("not a plain directory"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_inside_namespace_is_rejected_by_measurement() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let session = "symlink-inside-0000-0000-0000-000000000002";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let namespace = session_scratch_dir(&scratch_root, Some(session));
    symlink(temp.path().join("outside"), namespace.join("escape"))?;

    let error = measure_scratch_usage(&scratch_root, session)
        .expect_err("symlink escape inside the namespace must be rejected");
    assert!(format!("{error:#}").contains("symlink"));
    Ok(())
}

#[test]
fn deeply_nested_namespace_remains_measurable_and_does_not_poison_siblings() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let session_a = "deep-session-a-0000-0000-0000-000000000021";
    let session_b = "deep-session-b-0000-0000-0000-000000000022";
    ensure_session_scratch(&scratch_root, Some(session_a), &ScratchQuota::default())?;

    let mut nested = session_scratch_dir(&scratch_root, Some(session_a));
    for depth in 0..32 {
        nested = nested.join(format!("level-{depth}"));
    }
    fs::create_dir_all(&nested)?;
    fs::write(nested.join("payload"), b"measured")?;

    let usage = measure_scratch_usage(&scratch_root, session_a)?;
    assert_eq!(usage.session_bytes, 8);
    assert_eq!(usage.session_entry_count, 1);

    let provision_b =
        ensure_session_scratch(&scratch_root, Some(session_b), &ScratchQuota::default())?;
    assert_eq!(provision_b.usage.session_bytes, 0);
    assert_eq!(provision_b.usage.workspace_bytes, 8);
    Ok(())
}

#[test]
fn quota_exceeded_is_structured_and_releases_deterministically() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let session = "quota-session-0000-0000-0000-000000000003";
    let quota = ScratchQuota {
        per_session_bytes: 16,
        workspace_hard_bytes: 1024 * 1024,
    };
    ensure_session_scratch(&scratch_root, Some(session), &quota)?;
    let namespace = session_scratch_dir(&scratch_root, Some(session));
    fs::write(namespace.join("blob"), vec![b'x'; 32])?;

    let error = ensure_session_scratch(&scratch_root, Some(session), &quota)
        .expect_err("over-quota session must fail provisioning");
    let quota_error = error
        .downcast_ref::<ScratchQuotaExceededError>()
        .expect("quota failure must be the structured error");
    assert_eq!(quota_error.scope, ScratchQuotaScope::Session);
    assert_eq!(quota_error.usage_bytes, 32);
    assert_eq!(quota_error.quota_bytes, 16);
    assert!(quota_error.to_string().contains("reset scratch storage"));

    // Releasing space makes the next provision succeed deterministically.
    fs::remove_file(namespace.join("blob"))?;
    let provision = ensure_session_scratch(&scratch_root, Some(session), &quota)?;
    assert_eq!(provision.usage.session_bytes, 0);
    Ok(())
}

#[test]
fn workspace_hard_cap_spans_all_session_namespaces() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let quota = ScratchQuota {
        per_session_bytes: 1024 * 1024,
        workspace_hard_bytes: 16,
    };
    let session_a = "hard-cap-a-0000-0000-0000-000000000004";
    let session_b = "hard-cap-b-0000-0000-0000-000000000005";
    ensure_session_scratch(&scratch_root, Some(session_a), &quota)?;
    fs::write(
        session_scratch_dir(&scratch_root, Some(session_a)).join("blob"),
        vec![b'y'; 24],
    )?;

    let error = ensure_session_scratch(&scratch_root, Some(session_b), &quota)
        .expect_err("workspace hard cap must reject a second session namespace");
    let quota_error = error
        .downcast_ref::<ScratchQuotaExceededError>()
        .expect("quota failure must be the structured error");
    assert_eq!(quota_error.scope, ScratchQuotaScope::Workspace);
    assert_eq!(quota_error.usage_bytes, 24);
    assert_eq!(quota_error.quota_bytes, 16);
    Ok(())
}

#[test]
fn gc_deletes_expired_skips_leased_and_recent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let control = ScratchNamespaceControl::new();
    let quota = ScratchQuota::default();
    let config = ScratchGcConfig { ttl_ms: 5_000 };
    let now = unix_now_ms();

    let expired = "expired-session-0000-0000-0000-000000000006";
    let recent = "recent-session-0000-0000-0000-000000000007";
    let leased = "leased-session-0000-0000-0000-000000000008";
    for session in [expired, recent, leased] {
        ensure_session_scratch(&scratch_root, Some(session), &quota)?;
        fs::write(
            session_scratch_dir(&scratch_root, Some(session)).join("blob"),
            b"data",
        )?;
    }
    let lease = control
        .namespaces
        .acquire(&session_scratch_key(Some(leased)));

    #[cfg(unix)]
    {
        set_modified_recursively(&session_scratch_dir(&scratch_root, Some(expired)), 60_000)?;
        set_modified_recursively(&session_scratch_dir(&scratch_root, Some(recent)), 1_000)?;
    }

    let report = gc_scratch_namespaces(&scratch_root, &control, &config, now)?;
    // The lease check runs on every platform before any TTL decision.
    assert_eq!(report.skipped_leased, 1);
    #[cfg(unix)]
    {
        assert_eq!(report.scanned, 3);
        assert_eq!(report.deleted, 1);
        assert_eq!(report.skipped_recent, 1);
        assert_eq!(report.deleted_bytes, 4);
        assert!(!session_scratch_dir(&scratch_root, Some(expired)).exists());
        assert!(session_scratch_dir(&scratch_root, Some(recent)).exists());
    }
    assert!(session_scratch_dir(&scratch_root, Some(leased)).exists());
    drop(lease);
    Ok(())
}

#[test]
fn gc_skips_namespace_with_active_task_lease() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let control = ScratchNamespaceControl::new();
    let session = "task-lease-session-0000-0000-0000-000000000009";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let key = session_scratch_key(Some(session));
    control
        .tasks
        .register("terminal-1", &key, &control.namespaces);
    assert!(control.tasks.is_leased("terminal-1"));

    let report = gc_scratch_namespaces(
        &scratch_root,
        &control,
        &ScratchGcConfig { ttl_ms: 0 },
        unix_now_ms(),
    )?;
    assert_eq!(report.skipped_leased, 1);
    assert_eq!(report.deleted, 0);
    assert!(session_scratch_dir(&scratch_root, Some(session)).exists());

    control.tasks.release("terminal-1");
    assert!(!control.tasks.is_leased("terminal-1"));
    let report = gc_scratch_namespaces(
        &scratch_root,
        &control,
        &ScratchGcConfig { ttl_ms: 0 },
        unix_now_ms() + 10_000,
    )?;
    assert_eq!(report.deleted, 1);
    assert!(!session_scratch_dir(&scratch_root, Some(session)).exists());
    Ok(())
}

#[test]
fn delete_session_namespace_respects_active_lease() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let control = ScratchNamespaceControl::new();
    let session = "delete-session-0000-0000-0000-000000000010";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    let key = session_scratch_key(Some(session));

    let lease = control.namespaces.acquire(&key);
    assert_eq!(
        delete_session_scratch_namespace(&scratch_root, Some(session), &control)?,
        crate::scratch_namespace::ScratchDeleteOutcome::SkippedLeased
    );
    assert!(session_scratch_dir(&scratch_root, Some(session)).exists());
    drop(lease);

    assert_eq!(
        delete_session_scratch_namespace(&scratch_root, Some(session), &control)?,
        crate::scratch_namespace::ScratchDeleteOutcome::Deleted
    );
    assert!(!session_scratch_dir(&scratch_root, Some(session)).exists());
    assert_eq!(
        delete_session_scratch_namespace(&scratch_root, Some(session), &control)?,
        crate::scratch_namespace::ScratchDeleteOutcome::NotPresent
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_scratch_namespace_is_owner_only() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let session = "owner-only-session-0000-0000-0000-000000000011";
    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;

    for path in [
        scratch_root.as_path(),
        scratch_root.join("sessions").as_path(),
        session_scratch_dir(&scratch_root, Some(session)).as_path(),
    ] {
        let mode = fs::symlink_metadata(path)?.permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "scratch dir must not grant group/other: {path:?}"
        );
        assert!(
            mode & 0o700 == 0o700,
            "scratch dir must grant owner rwx: {path:?}"
        );
    }
    assert!(private_path_permissions_are_restricted(&scratch_root)?);
    Ok(())
}

/// RFC-0062 16.2 (Windows CI only): the session scratch namespace must narrow an explicit wide
/// parent DACL before any child can write into it. Different-SID second-principal access is not
/// claimed here; it requires a helper process under another account and is out of scope.
#[cfg(windows)]
#[test]
fn windows_scratch_namespace_narrows_wide_parent_dacl() -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let session = "windows-acl-session-0000-0000-0000-000000000012";

    {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            SetFileSecurityW,
        };
        fs::create_dir_all(&scratch_root)?;
        let wide = std::ffi::OsStr::new("D:(A;;FA;;;WD)")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "SDDL conversion failed");
        unsafe {
            let applied = SetFileSecurityW(
                scratch_root
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>()
                    .as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            );
            assert_ne!(applied, 0, "wide parent DACL apply failed");
            LocalFree(descriptor as _);
        }
    }
    assert!(
        !private_path_permissions_are_restricted(&scratch_root)?,
        "the fixture parent must be wide for this test to prove anything"
    );

    ensure_session_scratch(&scratch_root, Some(session), &ScratchQuota::default())?;
    assert!(private_path_permissions_are_restricted(
        &session_scratch_dir(&scratch_root, Some(session))
    )?);
    Ok(())
}

#[test]
fn lease_registry_delete_runs_under_the_registry_lock() -> Result<()> {
    let control = ScratchNamespaceControl::new();
    let key = "lock-race-session-0000-0000-0000-000000000013";
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("marker");
    fs::write(&path, b"x")?;

    let lease = control.namespaces.acquire(key);
    let deleted = control
        .namespaces
        .delete_if_unleased(key, || {
            fs::remove_file(&path)?;
            Ok(())
        })
        .expect("delete_if_unleased must not fail");
    assert!(!deleted, "leased key must never be deleted");
    assert!(path.exists());
    drop(lease);

    let deleted = control
        .namespaces
        .delete_if_unleased(key, || {
            fs::remove_file(&path)?;
            Ok(())
        })
        .expect("delete_if_unleased must not fail");
    assert!(deleted, "unleased key must be deleted");
    assert!(!path.exists());
    Ok(())
}

#[test]
fn scratch_task_lease_registry_is_arc_sharable() -> Result<()> {
    let namespaces = Arc::new(crate::scratch_namespace::ScratchNamespaceLeaseRegistry::new());
    let tasks = Arc::new(ScratchTaskLeaseRegistry::new());
    let key = "arc-share-session-0000-0000-0000-000000000014";
    tasks.register("terminal-arc", key, &namespaces);
    assert!(tasks.is_leased("terminal-arc"));
    assert!(namespaces.is_leased(key));
    tasks.release("terminal-arc");
    assert!(!namespaces.is_leased(key));
    Ok(())
}

#[test]
fn scratch_usage_defaults_to_zero_for_missing_root() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scratch_root = temp.path().join("missing").join("tmp");
    let usage = measure_scratch_usage(&scratch_root, "no-such-session")?;
    assert_eq!(usage, ScratchUsage::default());
    Ok(())
}
