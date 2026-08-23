//! RFC-0071: sigil-resource-authority (R71.2 bootstrap / lifecycle foundation).
//!
//! The only constructor for SandboxBoundExecutionLeaseV1::issue_prepared_launch lives here
//! (spawn_protocol.rs); sigil-sandbox submits factory-attested evidence and a one-shot actor
//! sink, and never imports authority local types in the reverse direction.

pub mod arena;
pub mod bootstrap;
pub mod identity;
pub mod journal;
pub mod lease;
pub mod maintenance;
pub mod provider_registry;
pub mod quota;
pub mod spawn_protocol;

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
