use super::*;

#[test]
fn attachment_is_exclusive_and_released_on_drop() -> anyhow::Result<()> {
    use anyhow::Context as _;

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-a.jsonl");
    let owner = InteractiveSessionAttachmentLease::acquire(&session_path)
        .context("initial attachment acquisition must succeed")?;
    assert_eq!(
        owner.session_path(),
        sigil_kernel::JsonlSessionStore::new(&session_path)?.path()
    );
    let busy = InteractiveSessionAttachmentLease::acquire(&session_path)
        .expect_err("second writer must be rejected");
    assert_eq!(busy.code(), SESSION_ATTACHMENT_BUSY_CODE);
    assert!(matches!(
        busy,
        InteractiveSessionAttachmentError::Busy { observed_generation }
            if observed_generation == owner.generation()
    ));
    drop(owner);
    InteractiveSessionAttachmentLease::acquire(&session_path)
        .context("attachment must be available immediately after owner drop")?;
    Ok(())
}

#[test]
fn attachment_recovery_binding_is_exact_to_session_and_generation() {
    let first = session_attachment_recovery_binding("session-a", "generation-1");
    assert_eq!(
        first,
        session_attachment_recovery_binding("session-a", "generation-1")
    );
    assert_ne!(
        first,
        session_attachment_recovery_binding("session-b", "generation-1")
    );
    assert_ne!(
        first,
        session_attachment_recovery_binding("session-a", "generation-2")
    );
    assert!(!first.contains("session-a"));
    assert!(!first.contains("generation-1"));
}

#[test]
fn path_retry_rejects_stale_generation_and_cross_session_binding() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let first_path = temp.path().join("session-retry-a.jsonl");
    let second_path = temp.path().join("session-retry-b.jsonl");
    let first_owner = InteractiveSessionAttachmentLease::acquire(&first_path)?;
    let stale_binding =
        session_attachment_path_recovery_binding(&first_path, first_owner.generation());
    drop(first_owner);

    let replacement_owner = InteractiveSessionAttachmentLease::acquire(&first_path)?;
    let replacement_binding =
        session_attachment_path_recovery_binding(&first_path, replacement_owner.generation());
    drop(replacement_owner);

    assert!(matches!(
        InteractiveSessionAttachmentLease::acquire_for_path_retry(&first_path, &stale_binding,),
        Err(InteractiveSessionAttachmentError::StaleRecoveryBinding {
            recovery_binding,
        }) if recovery_binding == replacement_binding
    ));
    let exact = InteractiveSessionAttachmentLease::acquire_for_path_retry(
        &first_path,
        &replacement_binding,
    )?;
    drop(exact);

    assert!(matches!(
        InteractiveSessionAttachmentLease::acquire_for_path_retry(
            &second_path,
            &replacement_binding,
        ),
        Err(InteractiveSessionAttachmentError::StaleRecoveryBinding { .. })
    ));
    Ok(())
}

#[test]
fn attachment_shares_route_authority_with_live_execution_owners() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-authority.jsonl");
    let attachment = InteractiveSessionAttachmentLease::acquire(&session_path)?;
    let first = attachment.route_mutation_authority("session-scope-a")?;
    let second = attachment.route_mutation_authority("session-scope-a")?;
    let owner = first.acquire_execution_owner()?;
    assert!(matches!(
        second.issue_quiescence_permit(),
        Err(crate::provider_connections::SessionRouteAuthorityError::ActiveOwners)
    ));
    drop(owner);
    second.issue_quiescence_permit()?;
    assert!(
        attachment
            .route_mutation_authority("session-scope-b")
            .is_err()
    );
    Ok(())
}
