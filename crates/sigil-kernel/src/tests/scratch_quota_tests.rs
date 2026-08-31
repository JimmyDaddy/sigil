use super::{ScratchQuotaExceededError, ScratchQuotaScope};

#[test]
fn scratch_quota_scope_labels_preserve_existing_error_details() {
    for (scope, label) in [
        (ScratchQuotaScope::Session, "session"),
        (ScratchQuotaScope::Workspace, "workspace"),
    ] {
        assert_eq!(scope.as_str(), label);
        assert_eq!(scope.to_string(), label);
    }
}

#[test]
fn scratch_quota_error_preserves_full_counts_through_context() {
    fn assert_shared_error<T: std::error::Error + Send + Sync + 'static>() {}
    assert_shared_error::<ScratchQuotaExceededError>();

    for scope in [ScratchQuotaScope::Session, ScratchQuotaScope::Workspace] {
        let quota = ScratchQuotaExceededError {
            scope,
            usage_bytes: u64::MAX,
            quota_bytes: u64::MAX - 1,
        };
        let error = anyhow::Error::new(quota).context("scratch admission failed");
        assert_eq!(
            error.downcast_ref::<ScratchQuotaExceededError>(),
            Some(&quota)
        );
        assert_eq!(
            quota.to_string(),
            format!(
                "scratch quota exceeded ({scope}): {} bytes used of {} bytes allowed",
                u64::MAX,
                u64::MAX - 1
            )
        );
        assert!(!quota.to_string().contains("reset"));
    }
}
