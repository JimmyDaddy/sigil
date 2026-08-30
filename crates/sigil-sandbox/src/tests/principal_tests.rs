use super::*;

#[test]
fn r71_windows_preview_is_restricted_token_and_never_leaks_sid() {
    let preview = windows_principal_preview();
    assert_eq!(
        preview.principal_class,
        SandboxPrincipalClassV1::RestrictedToken
    );
}

#[test]
fn r71_container_strategy_hashes_are_distinct() {
    assert_ne!(
        container_principal_digest(ContainerPrincipalBindingStrategyV1::KeepIdUserns),
        container_principal_digest(ContainerPrincipalBindingStrategyV1::Unsupported)
    );
}

#[test]
fn r71_unmapped_principal_cannot_bind() {
    let preview = SandboxPrincipalPreviewV1 {
        principal_class: SandboxPrincipalClassV1::Unmapped,
        capability_hash: CanonicalHash::from_bytes([0u8; 32]),
        preview_hash: CanonicalHash::from_bytes([0u8; 32]),
    };
    let error = validate_principal_binding(&preview, PrincipalBindingStateV1::Bound, 0)
        .expect_err("unmapped cannot bind");
    assert!(matches!(error, PrincipalBindingErrorV1::StrategyUnproven));
}
