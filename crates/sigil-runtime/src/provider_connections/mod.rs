mod catalog;
mod catalog_cache;
mod config;
mod configured_store;
mod credential;
mod dto;
mod file_store;
mod inventory;
mod keyring_store;
mod loader;
mod persistence;
mod recent;
mod route;
mod setup;

#[cfg(test)]
pub(crate) use catalog::seed_unauthenticated_catalog_cache_for_test;
pub use catalog::{
    ConnectionProbeResult, ConnectionProbeState, ModelAvailability, ModelCatalogEntry,
    ModelCatalogProvenance, ModelCatalogRequest, ModelCatalogResult, ModelCatalogState,
    ModelRecommendation, ProviderModelCatalogService, bundled_model_entries,
    connection_semantic_fingerprint, fresh_cached_model_entries_native,
};
pub use config::{
    CredentialId, CredentialRefConfig, ProviderConnectionConfig, ProviderFamily, ProviderProtocol,
    provider_connection_template,
};
pub use configured_store::ConfiguredProviderCredentialStore;
pub use credential::{
    CredentialAuthKind, CredentialEnvironment, CredentialGenerationId, LoadedCredentialRef,
    PreparedCredential, ProcessCredentialEnvironment, ProviderCredentialError,
    ProviderCredentialErrorCode, ProviderCredentialRecord, ProviderCredentialStore,
    ResolvedCredential, ResolvedCredentialSource, resolve_connection_credential,
};
pub use dto::{
    ConfigMode, ConnectionConfigIssue, ConnectionInventory, ConnectionInventoryEntry,
    ConnectionIssueView, ConnectionReadiness, CredentialSourceView, LoadedConnection,
    LoadedProviderConnections,
};
pub use file_store::FileProviderCredentialStore;
pub use inventory::{
    connection_inventory, connection_inventory_native, connection_inventory_offline,
    connection_inventory_with_cancellation,
};
pub use keyring_store::SystemProviderCredentialStore;
pub use loader::{load_provider_connections, materialize_root_config};
pub use persistence::{
    ConfigPublishOutcome, ConnectionCredentialUpdate, ConnectionSaveDraft, ConnectionSaveError,
    ConnectionSaveOutcome, ProviderConfigPublisher, RootConfigPublisher, save_connection_config,
    save_connection_config_replacing_invalid, save_connection_config_with_base,
};
pub use recent::{load_recent_model_refs, recent_models_path, record_recent_model_ref};
pub use route::{
    InspectedSessionRouteResume, ModelRouteSetupReason, ResolvedRouteConfigSnapshot,
    ResolvedRouteError, SessionRouteAuthorityError, SessionRouteConfirmationReason,
    SessionRouteExecutionOwner, SessionRouteLoadError, SessionRouteLoadOutcome,
    SessionRouteMutationAuthority, SessionRouteMutationPermit, SessionRouteRebindReason,
    SessionRouteResumeError, SessionRouteResumeInput, SessionRouteResumeOutcome,
    SessionRouteResumePlan, SessionRouteResumeStatus, SessionRouteTransitionKind,
    SessionRouteTransitionView, SessionRouteUnavailableReason,
    apply_explicit_session_route_selection, apply_session_route_confirmation_plan,
    apply_session_route_resume_plan, connection_egress_trust_binding, ensure_route_is_current,
    inspect_session_for_route_resume, load_session_for_route_resume,
    load_session_for_route_resume_with_directive,
    load_session_for_route_resume_with_directive_and_attachment,
    load_session_for_route_resume_with_directive_and_attachment_transition,
    plan_session_route_resume, resolve_default_model_route, resolve_model_route,
    runtime_provider_name, session_route_authority_generation_binding,
    session_route_frontier_binding, validate_persisted_model_route,
};
pub use setup::default_setup_root_config;

#[cfg(test)]
#[path = "../tests/provider_connections_tests.rs"]
mod tests;
