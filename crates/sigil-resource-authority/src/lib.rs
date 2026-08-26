//! RFC-0071: sigil-resource-authority (R71.2 bootstrap / lifecycle foundation).
//!
//! The only constructor for SandboxBoundExecutionLeaseV1::issue_prepared_launch lives here
//! (spawn_protocol.rs); sigil-sandbox submits factory-attested evidence and a one-shot actor
//! sink, and never imports authority local types in the reverse direction.

pub mod arena;
pub mod bootstrap;
pub mod borrowed;
pub mod configuration;
pub mod consumer_ports;
mod durable_snapshot;
pub mod factory;
pub mod file_access;
pub mod file_access_stub;
pub mod identity;
pub mod journal;
pub mod lease;
pub mod maintenance;
pub mod native_save;
pub mod provider_registry;
pub mod quota;
pub mod reconcile;
pub mod release_output;
pub mod semantic_matrix;
pub mod session_scratch;
pub mod spawn_protocol;
pub mod storage;

pub use bootstrap::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapRoots, BootstrapErrorV1,
    BootstrapRootResolverV1,
};
pub use journal::{JournalErrorV1, ResourceJournalEventV1, ResourceJournalHeaderV1};
pub use lease::{
    LeaseTransitionErrorV1, ManagedGenerationRecordV1, ManagedLeaseHandleV1,
    ResourceGenerationStateV1,
};
pub use quota::{QuotaBookV1, QuotaErrorV1, QuotaReservationV1};
pub use spawn_protocol::{PreparedSandboxLaunchV1, SandboxBoundExecutionLeaseV1};

#[cfg(test)]
#[path = "tests/fault_journal_tests.rs"]
mod fault_journal_tests;

#[cfg(test)]
#[path = "tests/fault_bootstrap_tests.rs"]
mod fault_bootstrap_tests;

#[cfg(test)]
#[path = "tests/fault_recovery_tests.rs"]
mod fault_recovery_tests;

#[cfg(test)]
#[path = "tests/fault_authority_bootstrap_tests.rs"]
mod fault_authority_bootstrap_tests;

#[cfg(test)]
#[path = "tests/fault_key_tests.rs"]
mod fault_key_tests;

#[cfg(test)]
#[path = "tests/fault_retire_tests.rs"]
mod fault_retire_tests;

#[cfg(test)]
#[path = "tests/fault_bridge_tests.rs"]
mod fault_bridge_tests;

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
#[path = "tests/fault_attachment_tests.rs"]
mod fault_attachment_tests;

#[cfg(test)]
#[path = "tests/fault_export_tests.rs"]
mod fault_export_tests;
