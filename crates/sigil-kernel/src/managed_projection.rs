//! RFC-0071 section 8.6: managed projection connection contract.
//!
//! SQLite-like databases cannot migrate through append/atomic blobs. The RebuildableDatabase
//! projection uses a pathless connection granted by the authority-owned projection service; the
//! runtime adapter holds only the exact handle and the connection, and never names the private
//! facet, authority token, descriptor or raw lease. The RA connection installs a SQLite
//! authorizer that rejects ATTACH/DETACH, extension loading, VACUUM INTO, path-bearing PRAGMA,
//! temp-store redirection and unregistered virtual tables/filesystem functions.

use std::collections::BTreeSet;

use crate::resource::CanonicalHash;

/// Closed prepared-statement identity (never raw SQL from a consumer).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectionStatementIdV1(String);

impl ProjectionStatementIdV1 {
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed projection statement: a typed AST / prepared-statement id plus bound-safe parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionStatementV1 {
    pub statement: ProjectionStatementIdV1,
    pub parameters: Vec<ProjectionParameterV1>,
    pub parameter_digest: CanonicalHash,
}

/// Closed parameter kinds (bounded; never raw SQL fragments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionParameterV1 {
    Text(String),
    Integer(i64),
    Unsigned(u64),
    Blob(CanonicalHash),
}

/// Connection request (pathless).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenProjectionConnectionRequestV1 {
    pub namespace_hash: CanonicalHash,
    pub capability_family: crate::resource::ManagedStorageCapabilityFamilyV1,
    pub semantic_owner: crate::resource::ManagedStorageSemanticOwnerV1,
}

/// Bounded row view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRowV1 {
    pub fields: Vec<ProjectionFieldV1>,
    pub row_digest: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionFieldV1 {
    Text(String),
    Integer(i64),
    Unsigned(u64),
    Null,
}

/// Read outcome: bounded rows plus receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionReadOutcomeV1 {
    pub rows: Vec<ProjectionRowV1>,
    pub receipt_hash: CanonicalHash,
}

/// Closed SQLite authorizer decision (RA connection installs this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionAuthorizerDecisionV1 {
    AllowRead,
    AllowWrite,
    Reject,
}

/// Closed unsafe-statement classification (authorizer rejects before execution).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafeProjectionClassV1 {
    AttachDetach,
    ExtensionLoading,
    VacuumInto,
    PathBearingPragma,
    TempStoreRedirection,
    UnregisteredVirtualTable,
    UnregisteredFilesystemFunction,
}

/// Authority-owned projection connection (non-clone, pathless).
#[async_trait::async_trait]
pub trait ManagedProjectionConnectionV1: Send {
    fn namespace_hash(&self) -> CanonicalHash;

    fn prepare(&mut self, statement: &ProjectionStatementV1) -> Result<(), ProjectionErrorV1>;

    async fn execute_read(
        &mut self,
        statement: &ProjectionStatementV1,
    ) -> Result<ProjectionReadOutcomeV1, ProjectionErrorV1>;

    async fn close(&mut self) -> Result<(), ProjectionErrorV1>;
}

/// Command wire that classifies unsafe SQLite operations for the authorizer.
pub fn classify_unsafe_projection(statement: &str) -> Option<UnsafeProjectionClassV1> {
    let lower = statement.to_lowercase();
    let contained = |needle: &str| lower.contains(needle);
    if contained("attach") || contained("detach") {
        return Some(UnsafeProjectionClassV1::AttachDetach);
    }
    if contained("load_extension")
        || contained(".load")
        || contained("pragma") && contained("load_extension")
    {
        return Some(UnsafeProjectionClassV1::ExtensionLoading);
    }
    if contained("vacuum into") {
        return Some(UnsafeProjectionClassV1::VacuumInto);
    }
    if contained("pragma")
        && (contained("database_list") || contained("journal_mode"))
        && !contained("read")
    {
        return Some(UnsafeProjectionClassV1::PathBearingPragma);
    }
    if contained("temp_store") {
        return Some(UnsafeProjectionClassV1::TempStoreRedirection);
    }
    None
}

/// Authority-owned projection service behind the factory.
#[async_trait::async_trait]
pub trait ManagedProjectionServiceV1: Send + Sync {
    async fn open_rebuildable_projection(
        &self,
        handle: &crate::managed_storage::ManagedStorageNamespaceHandleV1,
        request: OpenProjectionConnectionRequestV1,
    ) -> Result<Box<dyn ManagedProjectionConnectionV1>, ProjectionErrorV1>;
}

/// Closed projection error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionErrorV1 {
    #[error("handle capability family is not RebuildableDatabaseProjection")]
    FamilyMismatch,
    #[error("consumer statement is not a registered prepared statement")]
    UnregisteredStatement,
    #[error("consumer statement was rejected by the authorizer: {0:?}")]
    AuthorizerRejected(Option<UnsafeProjectionClassV1>),
    #[error("connection was closed or is stale")]
    ConnectionClosed,
    #[error("database rebuild requires an exact managed generation")]
    WrongGeneration,
}

/// Frozen prepared-statement registry (closed ids; consumers can never inject raw SQL).
pub fn registered_projection_statements() -> BTreeSet<ProjectionStatementIdV1> {
    let mut set = BTreeSet::new();
    for id in [
        "catalog.list_sessions",
        "catalog.session_by_id",
        "catalog.page_records",
        "code_intel.query_symbols",
    ] {
        set.insert(ProjectionStatementIdV1::new(id.to_owned()));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_projection_authorizer_rejects_attach_and_vacuum_into() {
        assert_eq!(
            classify_unsafe_projection("ATTACH DATABASE '/etc/passwd' AS x"),
            Some(UnsafeProjectionClassV1::AttachDetach)
        );
        assert_eq!(
            classify_unsafe_projection("VACUUM INTO '/tmp/out.sqlite'"),
            Some(UnsafeProjectionClassV1::VacuumInto)
        );
    }

    #[test]
    fn r71_projection_authorizer_allows_benign_reads() {
        assert_eq!(
            classify_unsafe_projection("SELECT id FROM sessions LIMIT 10"),
            None
        );
    }

    #[test]
    fn r71_projection_registry_is_closed() {
        let registry = registered_projection_statements();
        assert!(registry.contains(&ProjectionStatementIdV1::new(
            "catalog.list_sessions".to_owned()
        )));
        assert_eq!(registry.len(), 4);
    }
}
