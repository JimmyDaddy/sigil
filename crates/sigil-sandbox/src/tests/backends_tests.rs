use super::*;

#[test]
fn r71_backend_on_primary_platform_can_enforce_required_exact() {
    let declaration =
        SandboxBackendDeclarationV1::for_backend(SandboxBackendClassV1::MacOsSeatbelt, true);
    assert!(declaration.can_enforce_required_exact());
    assert_eq!(
        declaration.default_read_isolation,
        ReadIsolationCompletenessV1::Full
    );
}

#[test]
fn r71_backend_off_platform_is_unsupported() {
    let declaration =
        SandboxBackendDeclarationV1::for_backend(SandboxBackendClassV1::MacOsSeatbelt, false);
    assert_eq!(
        declaration.platform_support,
        SandboxPlatformSupportV1::Unsupported
    );
    assert!(!declaration.can_enforce_required_exact());
}

#[test]
fn r71_local_never_claims_filesystem_isolation() {
    let declaration =
        SandboxBackendDeclarationV1::for_backend(SandboxBackendClassV1::LocalUnconfined, true);
    assert!(!declaration.capabilities.filesystem_isolation);
    assert!(!declaration.can_enforce_required_exact());
    assert_eq!(declaration.enforcement, EnforcementCompletenessV1::None);
}
