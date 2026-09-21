//! Rebuildable, bounded-memory command lookup. The JSONL source is the authority.

use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

const INDEX_SCHEMA: u16 = 1;
const INDEX_RESERVATION_CHUNK: u64 = 4 * 1024 * 1024;
const INDEX_TABLES: [(&str, &str); 2] = [
    (
        "reservations",
        "CREATE TABLE reservations (key TEXT PRIMARY KEY, record TEXT NOT NULL) WITHOUT ROWID",
    ),
    (
        "checkpoint",
        "CREATE TABLE checkpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), value TEXT NOT NULL)",
    ),
];

pub(super) struct ReservationIndex {
    connection: Option<Connection>,
    offset: u64,
    sequence: u64,
    prefix: Sha256,
    namespace: String,
    binding: sigil_application::CommandJournalBinding,
    writable: bool,
    capacity: u64,
}

impl ReservationIndex {
    pub(super) fn is_writable(&self) -> bool {
        self.writable
    }
    pub(super) fn open(
        writer: &ManagedStorageWriterAdapterV1,
        source: &ManagedStorageWriterLeaseV1,
        index: &ManagedStorageWriterLeaseV1,
        binding: sigil_application::CommandJournalBinding,
        read_only: bool,
    ) -> Result<Self, ApplicationError> {
        let capacity = writer
            .reserve_index_open_capacity(index, INDEX_RESERVATION_CHUNK)
            .map_err(unavailable)?;
        let connection = writer
            .open_command_index(index, &INDEX_TABLES)
            .map_err(unavailable)?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS reservations (key TEXT PRIMARY KEY, record TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS checkpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), value TEXT NOT NULL);")
            .map_err(unavailable)?;
        let mut state = Self {
            connection: Some(connection),
            offset: 0,
            sequence: 0,
            prefix: Sha256::new(),
            namespace: source.namespace_digest().to_hex(),
            binding,
            writable: false,
            capacity,
        };
        // Always verify the canonical source. A checkpoint (even a well-formed one) is never
        // accepted merely because its offset or last record happens to match.
        state
            .connection()?
            .execute_batch("BEGIN IMMEDIATE; DELETE FROM reservations; DELETE FROM checkpoint;")
            .map_err(unavailable)?;
        let mut failure = None;
        let scanned = writer
            .scan_command_journal(source, !read_only, |line| {
                let result = state.replay_line(writer, index, line);
                if let Err(error) = result {
                    if !matches!(error, ApplicationError::CorruptProjection(_)) {
                        failure = Some(error);
                    }
                    return Err("invalid command journal record".to_owned());
                }
                Ok(())
            })
            .map_err(unavailable)?;
        if let Some(error) = failure {
            return Err(error);
        }
        state
            .connection()?
            .execute_batch("COMMIT")
            .map_err(unavailable)?;
        state.writable = scanned && !read_only;
        Ok(state)
    }

    fn connection(&self) -> Result<&Connection, ApplicationError> {
        self.connection
            .as_ref()
            .ok_or(ApplicationError::Unavailable)
    }

    pub(super) fn get(
        &self,
        key: &CommandReservationKey,
    ) -> Result<Option<ReservationRecord>, ApplicationError> {
        let encoded = encode(key)?;
        self.connection()?
            .query_row(
                "SELECT record FROM reservations WHERE key=?1",
                [encoded],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(unavailable)?
            .map(|record| serde_json::from_str(&record).map_err(unavailable))
            .transpose()
    }

    fn next_record(
        &self,
        operation: DurableReservationOperation,
    ) -> Result<(CommandReservationKey, ReservationRecord), ApplicationError> {
        let key = operation.key().clone();
        let mut single = BTreeMap::new();
        if let Some(record) = self.get(&key)? {
            single.insert(key.clone(), record);
        }
        apply_journal_operation(&mut single, operation)?;
        Ok((
            key.clone(),
            single.remove(&key).ok_or(ApplicationError::Unavailable)?,
        ))
    }

    fn reserve_capacity(
        &mut self,
        writer: &ManagedStorageWriterAdapterV1,
        lease: &ManagedStorageWriterLeaseV1,
        incoming: usize,
    ) -> Result<(), ApplicationError> {
        let pages: i64 = self
            .connection()?
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(unavailable)?;
        let page_size: i64 = self
            .connection()?
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(unavailable)?;
        let pages = u64::try_from(pages).map_err(unavailable)?;
        let page_size = u64::try_from(page_size).map_err(unavailable)?;
        if page_size == 0 {
            return Err(ApplicationError::Unavailable);
        }
        // Account for the rollback journal as well as the database. Growth remains an RA
        // storage reservation, not a command-history count limit.
        let needed = pages
            .saturating_mul(page_size)
            .saturating_mul(2)
            .saturating_add((incoming as u64).saturating_mul(4))
            .saturating_add(64 * 1024);
        if needed > self.capacity {
            let next = needed.div_ceil(INDEX_RESERVATION_CHUNK) * INDEX_RESERVATION_CHUNK;
            self.capacity = writer
                .reserve_index_capacity(lease, next)
                .map_err(unavailable)?;
        }
        let maximum_pages = self.capacity / page_size / 2;
        self.connection()?
            .pragma_update(
                None,
                "max_page_count",
                i64::try_from(maximum_pages).map_err(unavailable)?,
            )
            .map_err(unavailable)?;
        Ok(())
    }

    fn replay_line(
        &mut self,
        writer: &ManagedStorageWriterAdapterV1,
        index: &ManagedStorageWriterLeaseV1,
        line: &[u8],
    ) -> Result<(), ApplicationError> {
        if self.sequence == 0 && self.binding.command_generation != 0 {
            let header: super::recovery::ControlLogHeader =
                serde_json::from_slice(line).map_err(|_| corrupt())?;
            if header.schema_version != 1
                || header.kind != "application_control_header"
                || header.logical_journal_id != self.binding.logical_journal_id
                || header.command_generation != self.binding.command_generation
            {
                return Err(corrupt());
            }
            self.advance_checkpoint(line)?;
            return Ok(());
        }
        let entry: DurableReservationJournalEntry =
            serde_json::from_slice(line).map_err(|_| corrupt())?;
        if entry.schema_version != APPLICATION_RESERVATION_SCHEMA_VERSION {
            return Err(corrupt());
        }
        let (key, record) = self.next_record(entry.operation)?;
        self.reserve_capacity(writer, index, line.len())?;
        self.save_row(&key, &record)?;
        self.advance_checkpoint(line)?;
        Ok(())
    }

    fn save_row(
        &self,
        key: &CommandReservationKey,
        record: &ReservationRecord,
    ) -> Result<(), ApplicationError> {
        self.connection()?.execute("INSERT INTO reservations(key,record) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET record=excluded.record",
            params![encode(key)?, encode(record)?]).map_err(unavailable)?;
        Ok(())
    }

    fn advance_checkpoint(&mut self, line: &[u8]) -> Result<(), ApplicationError> {
        self.offset = self
            .offset
            .checked_add(line.len() as u64 + 1)
            .ok_or(ApplicationError::Unavailable)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ApplicationError::Unavailable)?;
        self.prefix.update(line);
        self.prefix.update(b"\n");
        let prefix_digest = format!("{:x}", self.prefix.clone().finalize());
        let last_record_digest = format!("{:x}", Sha256::digest(line));
        let checkpoint = serde_json::json!({ "schema": INDEX_SCHEMA,
            "logical_journal_id": self.binding.logical_journal_id, "command_generation": self.binding.command_generation,
            "source_namespace": self.namespace, "byte_offset": self.offset,
            "record_sequence": self.sequence, "prefix_digest": prefix_digest,
            "last_record_digest": last_record_digest });
        self.connection()?.execute("INSERT INTO checkpoint(singleton,value) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET value=excluded.value",
            [checkpoint.to_string()]).map_err(unavailable)?;
        Ok(())
    }

    pub(super) fn append(
        &mut self,
        writer: &ManagedStorageWriterAdapterV1,
        source: &ManagedStorageWriterLeaseV1,
        index: &ManagedStorageWriterLeaseV1,
        operation: DurableReservationOperation,
    ) -> Result<(), ApplicationError> {
        if !self.writable {
            return Err(ApplicationError::Unavailable);
        }
        let (key, record) = self.next_record(operation.clone())?;
        let line = serde_json::to_vec(&DurableReservationJournalEntry {
            schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
            operation,
        })
        .map_err(unavailable)?;
        self.reserve_capacity(writer, index, line.len())?;
        // A failed append/fsync or projection commit poisons this writer instance. Reopen must
        // verify the physical prefix before another forward operation is possible.
        self.writable = false;
        writer
            .append_command_record(source, self.offset, self.sequence, &line)
            .map_err(unavailable)?;
        self.connection()?
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(unavailable)?;
        self.save_row(&key, &record)?;
        self.advance_checkpoint(&line)?;
        self.connection()?
            .execute_batch("COMMIT")
            .map_err(unavailable)?;
        self.writable = true;
        Ok(())
    }

    pub(super) fn close(&mut self) {
        self.connection.take();
    }

    pub(super) fn recovery_impact(
        &self,
        physical_length: u64,
        prefix_digest: String,
    ) -> Result<sigil_application::ControlLogRecoveryImpact, ApplicationError> {
        use sigil_application::{
            ControlLogRecoveryImpact, ControlLogRecoveryScope, ControlLogUnresolvedCommand,
        };
        let verified_prefix_bytes = self.offset.min(physical_length);
        let mut impact = ControlLogRecoveryImpact {
            verified_prefix_bytes,
            verified_record_count: self.sequence,
            verified_prefix_digest: prefix_digest,
            known_command_count: 0,
            affected_scope_count: 0,
            affected_scopes: Vec::new(),
            scopes_truncated: false,
            known_unresolved_count: 0,
            unresolved_commands: Vec::new(),
            commands_truncated: false,
            unparsed_tail_bytes: physical_length - verified_prefix_bytes,
            tail_command_count_unknown: verified_prefix_bytes < physical_length,
        };
        let mut statement = self
            .connection()?
            .prepare("SELECT key,record FROM reservations ORDER BY key")
            .map_err(unavailable)?;
        let mut rows = statement.query([]).map_err(unavailable)?;
        let mut previous_scope = None;
        while let Some(row) = rows.next().map_err(unavailable)? {
            let key: CommandReservationKey =
                serde_json::from_str(&row.get::<_, String>(0).map_err(unavailable)?)
                    .map_err(unavailable)?;
            let record: ReservationRecord =
                serde_json::from_str(&row.get::<_, String>(1).map_err(unavailable)?)
                    .map_err(unavailable)?;
            let scope_json = encode(&key.authority_scope)?;
            let scope_digest = sigil_kernel::sha256_hex(scope_json.as_bytes());
            impact.known_command_count += 1;
            // Canonically encoded keys sort application_instance then authority_scope before
            // principal/epoch/command. Equal scopes are contiguous; no unbounded identity set.
            if previous_scope.as_ref() != Some(&scope_json) {
                impact.affected_scope_count += 1;
                if impact.affected_scopes.len() < 16 {
                    impact.affected_scopes.push(ControlLogRecoveryScope {
                        scope_digest: scope_digest.clone(),
                        session_id: key
                            .authority_scope
                            .session
                            .as_ref()
                            .map(|id| preview_label(id.as_str())),
                        workspace_id: key
                            .authority_scope
                            .workspace
                            .as_ref()
                            .map(|id| preview_label(id.as_str())),
                    });
                }
                previous_scope = Some(scope_json);
            }
            let unresolved = match &record.state {
                DurableReservationState::Reserved
                | DurableReservationState::DispatchStarted
                | DurableReservationState::EffectStarted(_)
                | DurableReservationState::Uncertain(_) => true,
                DurableReservationState::Settled(receipt) => matches!(
                    receipt.as_ref(),
                    ApplicationCommandReceipt::Uncertain(_)
                        | ApplicationCommandReceipt::ReplayedUncertain(_)
                ),
                _ => false,
            };
            if unresolved {
                impact.known_unresolved_count += 1;
                if impact.unresolved_commands.len() < 32 {
                    impact
                        .unresolved_commands
                        .push(ControlLogUnresolvedCommand {
                            key_digest: sigil_kernel::sha256_hex(encode(&key)?.as_bytes()),
                            scope_digest,
                            command_id: preview_label(key.command_id.as_str()),
                            command_kind: record
                                .request
                                .as_ref()
                                .map_or("legacy_unknown", |request| request.envelope.command.kind())
                                .to_owned(),
                            phase: phase_for_state(&record.state),
                        });
                }
            }
        }
        impact.scopes_truncated = impact.affected_scope_count > impact.affected_scopes.len() as u64;
        impact.commands_truncated =
            impact.known_unresolved_count > impact.unresolved_commands.len() as u64;
        Ok(impact)
    }

    pub(super) fn verified_prefix_bytes(&self, physical_length: u64) -> u64 {
        self.offset.min(physical_length)
    }
}

fn preview_label(value: &str) -> String {
    sigil_kernel::safe_persistence_text(value)
        .chars()
        .take(128)
        .collect()
}

impl DurableReservationOperation {
    fn key(&self) -> &CommandReservationKey {
        match self {
            Self::Reserve { key, .. }
            | Self::ReserveWithContextV1 { key, .. }
            | Self::DispatchStarted { key, .. }
            | Self::EffectStarted { key, .. }
            | Self::SessionRuntimeEffectResumedV1 { key, .. }
            | Self::DomainCommitted { key, .. }
            | Self::ConfirmedNoEffect { key, .. }
            | Self::Uncertain { key, .. }
            | Self::Settled { key, .. } => key,
        }
    }
}

fn encode(value: &impl Serialize) -> Result<String, ApplicationError> {
    serde_json::to_string(value).map_err(unavailable)
}
fn unavailable(_: impl fmt::Display) -> ApplicationError {
    ApplicationError::Unavailable
}
fn corrupt() -> ApplicationError {
    ApplicationError::CorruptProjection("application reservation journal is corrupt".to_owned())
}
