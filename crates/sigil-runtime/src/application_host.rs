//! Host-facing application boot bridge.
//!
//! Surfaces depend on this stable runtime composition boundary rather than the historical R71
//! module names. The authority implementation remains owned by runtime/R71; this module is only
//! a compatibility-free host composition API and does not create a second authority.

pub use crate::r71_authority_composition::{
    BootAuthorityErrorV1, RuntimeAuthorityCompositionErrorV1, RuntimeAuthorityCompositionV1,
    RuntimeCurrentBootTransactionV1, ValidatedAuthorityConfigSnapshotV1,
    attach_boot_authority_to_services, authority_bootstrap_manifest_path, boot_current_schema,
    boot_current_schema_with_expected_config,
};
pub use crate::r71_global_cutover::{
    CutoverSessionOpenErrorV1, RuntimeGlobalCutoverV1, guarded_session_open,
};
