//! RFC-0071 section 8.6 / R71.6: production rebuildable projection over managed records.
//!
//! SQLite-like catalog state cannot migrate through append/atomic blobs, so the runtime
//! rebuilds an exact-generation database from the authority-declared managed records on
//! open. The connection is pathless and closed-statement: only registered statement ids are
//! accepted, parameter values are type-bounded, and the authorizer never sees raw SQL from a
//! consumer. The physical records file stays in the authority-owned layout; this service
//! never derives a root from env/cwd.

use std::path::PathBuf;

use rusqlite::{Connection, params};
use sigil_kernel::managed_projection::{
    ManagedProjectionConnectionV1, ManagedProjectionServiceV1, OpenProjectionConnectionRequestV1,
    ProjectionErrorV1, ProjectionFieldV1, ProjectionParameterV1, ProjectionReadOutcomeV1,
    ProjectionRowV1, ProjectionStatementIdV1, ProjectionStatementV1,
};
use sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1;
use sigil_kernel::resource::{
    CanonicalHash, ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
};

/// Closed registered statement ids for the rebuildable projection.
mod statements {
    pub const SELECT_ALL: &str = "select-all";
    pub const SELECT_BY_SEQ: &str = "select-by-seq";
    pub const SELECT_BY_HASH: &str = "select-by-hash";
}

/// Verified leaf for a rebuildable owner (authority-layout, closed mapping).
fn leaf_for_owner(owner: ManagedStorageSemanticOwnerV1) -> Option<&'static str> {
    match owner {
        ManagedStorageSemanticOwnerV1::SessionCatalog => Some("session-catalog"),
        _ => None,
    }
}

fn digest_of(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Production rebuildable projection service (singleton per composition).
pub struct RuntimeRecordsProjectionServiceV1 {
    state_anchor: PathBuf,
}

impl RuntimeRecordsProjectionServiceV1 {
    pub fn new(state_anchor: PathBuf) -> Self {
        Self { state_anchor }
    }

    fn records_path(&self, leaf: &str) -> Result<PathBuf, ProjectionErrorV1> {
        let prefix = self
            .state_anchor
            .canonicalize()
            .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
        Ok(prefix.join("managed").join(leaf).join("records.jsonl"))
    }
}

#[async_trait::async_trait]
impl ManagedProjectionServiceV1 for RuntimeRecordsProjectionServiceV1 {
    async fn open_rebuildable_projection(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        request: OpenProjectionConnectionRequestV1,
    ) -> Result<Box<dyn ManagedProjectionConnectionV1>, ProjectionErrorV1> {
        if handle.capability_family
            != ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection
            || request.capability_family
                != ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection
        {
            return Err(ProjectionErrorV1::FamilyMismatch);
        }
        let Some(leaf) = leaf_for_owner(request.semantic_owner) else {
            return Err(ProjectionErrorV1::FamilyMismatch);
        };
        let records = self.records_path(leaf)?;
        let content =
            std::fs::read_to_string(&records).map_err(|_| ProjectionErrorV1::WrongGeneration)?;
        // Exact managed generation: the namespace hash must bind the rebuilt database.
        let conn = Connection::open_in_memory().map_err(|_| ProjectionErrorV1::WrongGeneration)?;
        conn.execute(
            "CREATE TABLE records (seq INTEGER PRIMARY KEY, payload BLOB NOT NULL, record_hash BLOB NOT NULL)",
            [],
        )
        .map_err(|_| ProjectionErrorV1::WrongGeneration)?;
        let mut seq: i64 = 0;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            seq += 1;
            let payload = line.as_bytes();
            let hash = digest_of(payload);
            conn.execute(
                "INSERT INTO records (seq, payload, record_hash) VALUES (?1, ?2, ?3)",
                params![seq, payload, hash.as_bytes()],
            )
            .map_err(|_| ProjectionErrorV1::WrongGeneration)?;
        }
        let _ = handle.namespace_hash;
        Ok(Box::new(RecordsProjectionConnectionV1 {
            conn,
            namespace_hash: handle.namespace_hash,
        }))
    }
}

/// Rebuilt-database connection (pathless; closed statement ids only).
struct RecordsProjectionConnectionV1 {
    conn: Connection,
    namespace_hash: CanonicalHash,
}

impl RecordsProjectionConnectionV1 {
    fn registered(&self, id: &ProjectionStatementIdV1) -> bool {
        matches!(
            id.as_str(),
            statements::SELECT_ALL | statements::SELECT_BY_SEQ | statements::SELECT_BY_HASH
        )
    }

    fn read_rows(
        &mut self,
        statement: &ProjectionStatementV1,
    ) -> Result<Vec<ProjectionRowV1>, ProjectionErrorV1> {
        let rows = match statement.statement.as_str() {
            statements::SELECT_ALL => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT seq, payload, record_hash FROM records ORDER BY seq")
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mapped = stmt
                    .query_map([], |row| {
                        let seq: i64 = row.get(0)?;
                        let payload: Vec<u8> = row.get(1)?;
                        let hash: Vec<u8> = row.get(2)?;
                        Ok((seq, payload, hash))
                    })
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mut out = Vec::new();
                for item in mapped {
                    let (seq, payload, hash) =
                        item.map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let mut fields = vec![ProjectionFieldV1::Integer(seq)];
                    fields.push(ProjectionFieldV1::Text(
                        String::from_utf8_lossy(&payload).into_owned(),
                    ));
                    let mut hex = String::new();
                    for byte in hash {
                        use std::fmt::Write;
                        let _ = write!(hex, "{byte:02x}");
                    }
                    fields.push(ProjectionFieldV1::Text(hex));
                    let mut material = Vec::new();
                    material.extend_from_slice(&seq.to_be_bytes());
                    material.extend_from_slice(&payload);
                    out.push(ProjectionRowV1 {
                        fields,
                        row_digest: digest_of(&material),
                    });
                    if out.len() >= 1000 {
                        break;
                    }
                }
                out
            }
            statements::SELECT_BY_SEQ => {
                let want = match statement.parameters.first() {
                    Some(ProjectionParameterV1::Integer(value)) => *value,
                    Some(ProjectionParameterV1::Unsigned(value)) => *value as i64,
                    _ => return Err(ProjectionErrorV1::UnregisteredStatement),
                };
                let mut stmt = self
                    .conn
                    .prepare("SELECT seq, payload, record_hash FROM records WHERE seq = ?1")
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mut rows = stmt
                    .query([want])
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mut out = Vec::new();
                while let Some(row) = rows
                    .next()
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?
                {
                    let seq: i64 = row
                        .get(0)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let payload: Vec<u8> = row
                        .get(1)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let hash: Vec<u8> = row
                        .get(2)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let mut fields = vec![ProjectionFieldV1::Integer(seq)];
                    fields.push(ProjectionFieldV1::Text(
                        String::from_utf8_lossy(&payload).into_owned(),
                    ));
                    let mut hex = String::new();
                    for byte in hash {
                        use std::fmt::Write;
                        let _ = write!(hex, "{byte:02x}");
                    }
                    fields.push(ProjectionFieldV1::Text(hex));
                    let mut material = Vec::new();
                    material.extend_from_slice(&seq.to_be_bytes());
                    material.extend_from_slice(&payload);
                    out.push(ProjectionRowV1 {
                        fields,
                        row_digest: digest_of(&material),
                    });
                }
                out
            }
            statements::SELECT_BY_HASH => {
                let want = match statement.parameters.first() {
                    Some(ProjectionParameterV1::Blob(hash)) => hash.as_bytes().to_vec(),
                    _ => return Err(ProjectionErrorV1::UnregisteredStatement),
                };
                let mut stmt = self
                    .conn
                    .prepare("SELECT seq, payload, record_hash FROM records WHERE record_hash = ?1")
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mut rows = stmt
                    .query([&want[..]])
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                let mut out = Vec::new();
                while let Some(row) = rows
                    .next()
                    .map_err(|_| ProjectionErrorV1::ConnectionClosed)?
                {
                    let seq: i64 = row
                        .get(0)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let payload: Vec<u8> = row
                        .get(1)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let hash: Vec<u8> = row
                        .get(2)
                        .map_err(|_| ProjectionErrorV1::ConnectionClosed)?;
                    let mut fields = vec![ProjectionFieldV1::Integer(seq)];
                    fields.push(ProjectionFieldV1::Text(
                        String::from_utf8_lossy(&payload).into_owned(),
                    ));
                    let mut hex = String::new();
                    for byte in hash {
                        use std::fmt::Write;
                        let _ = write!(hex, "{byte:02x}");
                    }
                    fields.push(ProjectionFieldV1::Text(hex));
                    let mut material = Vec::new();
                    material.extend_from_slice(&seq.to_be_bytes());
                    material.extend_from_slice(&payload);
                    out.push(ProjectionRowV1 {
                        fields,
                        row_digest: digest_of(&material),
                    });
                }
                out
            }
            _ => return Err(ProjectionErrorV1::UnregisteredStatement),
        };
        Ok(rows)
    }
}

#[async_trait::async_trait]
impl ManagedProjectionConnectionV1 for RecordsProjectionConnectionV1 {
    fn namespace_hash(&self) -> CanonicalHash {
        self.namespace_hash
    }

    fn prepare(&mut self, statement: &ProjectionStatementV1) -> Result<(), ProjectionErrorV1> {
        // Closed statement ids only: no raw SQL ever reaches the database.
        if !self.registered(&statement.statement) {
            return Err(ProjectionErrorV1::UnregisteredStatement);
        }
        Ok(())
    }

    async fn execute_read(
        &mut self,
        statement: &ProjectionStatementV1,
    ) -> Result<ProjectionReadOutcomeV1, ProjectionErrorV1> {
        let rows = self.read_rows(statement)?;
        let mut material = Vec::new();
        for row in &rows {
            material.extend_from_slice(row.row_digest.as_bytes());
        }
        Ok(ProjectionReadOutcomeV1 {
            rows,
            receipt_hash: digest_of(&material),
        })
    }

    async fn close(&mut self) -> Result<(), ProjectionErrorV1> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/runtime_records_projection_tests.rs"]
mod tests;
