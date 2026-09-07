//! RFC-0071: sigil-resource-authority (R71.2 bootstrap / lifecycle foundation).
//!
//! The only constructor for SandboxBoundExecutionLeaseV1::issue_prepared_launch lives here
//! (spawn_protocol.rs); sigil-sandbox submits factory-attested evidence and a one-shot actor
//! sink, and never imports authority local types in the reverse direction.

pub mod arena;
pub mod bootstrap;
pub mod borrowed;
pub mod configuration;
#[cfg(test)]
pub mod consumer_ports;
mod durable_snapshot;
pub mod factory;
pub mod file_access;
#[cfg(any(test, feature = "test-support"))]
pub mod file_access_stub;
pub mod identity;
#[cfg(test)]
pub mod lease;
pub mod maintenance;
pub mod native_save;
pub mod process_inventory;
pub mod quota;
pub mod release_output;
#[cfg(test)]
pub mod semantic_matrix;
pub mod session_scratch;
pub mod spawn_protocol;
pub mod storage;

pub use bootstrap::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapPublicationGuard, AuthorityBootstrapRoots,
    AuthorityBootstrapStoreV1, BootstrapErrorV1, BootstrapRootResolverV1,
};
#[cfg(test)]
pub use lease::{
    LeaseTransitionErrorV1, ManagedGenerationRecordV1, ManagedLeaseHandleV1,
    ResourceGenerationStateV1,
};
#[cfg(feature = "test-support")]
pub use process_inventory::InMemoryAuthorityProcessInventoryV1;
pub use process_inventory::{
    AuthorityManagedProcessInventoryV1, AuthorityProcessInventoryBootstrapBindingV1,
    AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1,
    AuthorityProcessInventoryPortV1, AuthorityProcessSpawnRequestV1,
};
pub use quota::{QuotaBookV1, QuotaErrorV1, QuotaReservationV1};
pub use spawn_protocol::{PreparedSandboxLaunchV1, SandboxBoundExecutionLeaseV1};

#[cfg(test)]
#[path = "tests/fault_bootstrap_tests.rs"]
mod fault_bootstrap_tests;

#[cfg(test)]
#[path = "tests/fault_attachment_tests.rs"]
mod fault_attachment_tests;

#[cfg(test)]
#[path = "tests/fault_key_tests.rs"]
mod fault_key_tests;

#[cfg(test)]
#[path = "tests/fault_retire_tests.rs"]
mod fault_retire_tests;

#[cfg(test)]
#[path = "tests/fault_child_tests.rs"]
mod fault_child_tests;

#[cfg(test)]
#[path = "tests/fault_updater_tests.rs"]
mod fault_updater_tests;

#[cfg(test)]
#[path = "tests/fault_borrowed_tests.rs"]
mod fault_borrowed_tests;

#[cfg(test)]
#[path = "tests/fault_mutation_tests.rs"]
mod fault_mutation_tests;

#[cfg(test)]
#[path = "tests/fault_catalog_tests.rs"]
mod fault_catalog_tests;

#[cfg(test)]
#[path = "tests/fault_export_tests.rs"]
mod fault_export_tests;
