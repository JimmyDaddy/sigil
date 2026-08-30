//! Host-facing application boot bridge.
//!
//! Surfaces depend on this stable runtime composition boundary rather than the historical R71
//! module names. The authority implementation remains owned by runtime/R71; this module is only
//! a compatibility-free host composition API and does not create a second authority.

pub use crate::r71_authority_composition::{
    BootAuthorityErrorV1, RuntimeAuthorityCompositionV1, RuntimeCurrentBootTransactionV1,
    ValidatedAuthorityConfigSnapshotV1, authority_bootstrap_manifest_path,
};
pub use crate::r71_global_cutover::{
    CutoverSessionOpenErrorV1, RuntimeGlobalCutoverV1, guarded_session_open,
};

pub fn boot_current_schema(
    config_path: &std::path::Path,
    launch_cwd: &std::path::Path,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    crate::r71_authority_composition::boot_current_schema(config_path, launch_cwd)
}

pub fn attach_boot_authority_to_services(
    services: crate::application_run::ApplicationRunServices,
    config_path: &std::path::Path,
    launch_cwd: &std::path::Path,
) -> Result<crate::application_run::ApplicationRunServices, BootAuthorityErrorV1> {
    crate::r71_authority_composition::attach_boot_authority_to_services(
        services,
        config_path,
        launch_cwd,
    )
}
