//! Explicit generation recovery; RA owns seals, initialization and durable phase transitions.

use super::*;
use sha2::{Digest, Sha256};
use sigil_application::{
    CommandJournalBinding, ControlLogRecoveryImpact, ControlLogRecoveryPreview,
};
use sigil_kernel::managed_storage::{ControlLogRecoveryPhaseV1, ControlLogRecoveryRequestV1};
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ControlLogHeader {
    pub schema_version: u16,
    #[serde(rename = "type")]
    pub kind: String,
    pub logical_journal_id: String,
    pub command_generation: u64,
    pub operation_id: String,
    pub old_namespace_hash: String,
    pub old_byte_length: u64,
    pub old_content_digest: String,
}

pub(super) fn generation_key(key: &str, generation: u64) -> String {
    if generation == 0 {
        return key.to_owned();
    }
    format!(
        "{}-g{generation}",
        &format!("{:x}", Sha256::digest(key.as_bytes()))[..40]
    )
}

impl ManagedApplicationReservationStore {
    fn recovery_impact(
        &self,
        entries: &mut ReservationIndex,
    ) -> Result<
        (
            ControlLogRecoveryImpact,
            u64,
            sigil_kernel::resource::CanonicalHash,
        ),
        ApplicationError,
    > {
        let source = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let source = source.as_ref().ok_or(ApplicationError::Unavailable)?;
        let index = self
            .index_lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let index = index.as_ref().ok_or(ApplicationError::Unavailable)?;
        let before = self
            .writer
            .command_physical_digest(source)
            .map_err(|_| ApplicationError::Unavailable)?;
        // Close any failed SQLite transaction before rebuilding this disposable index. Recovery
        // never repairs or appends canonical bytes, and keeps only bounded samples in memory.
        entries.close();
        let rebuilt = ReservationIndex::open(&self.writer, source, index, self.binding(), true)?;
        *entries = rebuilt;
        let prefix = self
            .writer
            .command_prefix_digest(source, entries.verified_prefix_bytes(before.0))
            .map_err(|_| ApplicationError::Unavailable)?;
        let impact = entries.recovery_impact(before.0, prefix.to_hex())?;
        if self
            .writer
            .command_physical_digest(source)
            .map_err(|_| ApplicationError::Unavailable)?
            != before
        {
            return Err(ApplicationError::Unavailable);
        }
        Ok((impact, before.0, before.1))
    }

    pub(super) fn binding(&self) -> CommandJournalBinding {
        CommandJournalBinding {
            logical_journal_id: self.logical_journal_id.to_hex(),
            command_generation: self.generation.load(Ordering::Acquire),
        }
    }

    fn header(
        &self,
        operation_id: String,
        successor_generation: u64,
        old_namespace_hash: String,
        old_byte_length: u64,
        old_content_digest: String,
    ) -> Result<Vec<u8>, ApplicationError> {
        let mut bytes = serde_json::to_vec(&ControlLogHeader {
            schema_version: 1,
            kind: "application_control_header".to_owned(),
            logical_journal_id: self.logical_journal_id.to_hex(),
            command_generation: successor_generation,
            operation_id,
            old_namespace_hash,
            old_byte_length,
            old_content_digest,
        })
        .map_err(|_| ApplicationError::Unavailable)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Produces an exact physical preview without changing any canonical bytes or generation.
    pub fn preview_control_log_recovery(
        &self,
    ) -> Result<ControlLogRecoveryPreview, ApplicationError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let recovery = self
            .recovery_lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let recovery = recovery.as_ref().ok_or(ApplicationError::Unavailable)?;
        if let Some(state) = self
            .writer
            .control_log_recovery_state(recovery)
            .map_err(|_| ApplicationError::Unavailable)?
            && (state.phase != ControlLogRecoveryPhaseV1::Activated
                || state.preview.request.successor_generation
                    > self.generation.load(Ordering::Acquire))
        {
            let (impact, _, _) = self.recovery_impact(&mut entries)?;
            if impact_digest(&impact)? != state.preview.request.owner_context_digest {
                return Err(ApplicationError::Unavailable);
            }
            return Ok(ControlLogRecoveryPreview {
                authority: state.preview,
                impact,
            });
        }
        if entries.is_writable() {
            return Err(ApplicationError::InvalidRequest(
                "command journal is writable; recovery is not required".to_owned(),
            ));
        }
        let (impact, old_byte_length, old_content_digest) = self.recovery_impact(&mut entries)?;
        let old = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let old = old.as_ref().ok_or(ApplicationError::Unavailable)?;
        let from_generation = self.generation.load(Ordering::Acquire);
        let successor_generation = from_generation
            .checked_add(1)
            .ok_or(ApplicationError::Unavailable)?;
        let successor = self
            .writer
            .acquire_named(
                StorageWriterChannelV1::ApplicationControlLog,
                &generation_key(&self.logical_key, successor_generation),
            )
            .map_err(|_| ApplicationError::Unavailable)?;
        let result = (|| {
            let operation_id = uuid::Uuid::new_v4().to_string();
            let header = self.header(
                operation_id.clone(),
                successor_generation,
                old.namespace_digest().to_hex(),
                old_byte_length,
                old_content_digest.to_hex(),
            )?;
            let request = ControlLogRecoveryRequestV1 {
                logical_journal_id: self.logical_journal_id,
                operation_id,
                from_generation,
                successor_generation,
                header_digest: sigil_kernel::resource::CanonicalHash::from_bytes(
                    Sha256::digest(&header).into(),
                ),
                owner_context_digest: impact_digest(&impact)?,
            };
            let preview = self
                .writer
                .preview_control_log_recovery(recovery, old, &successor, request)
                .map_err(|_| ApplicationError::Unavailable)?;
            if preview.old_byte_length != old_byte_length
                || preview.old_content_digest != old_content_digest
            {
                return Err(ApplicationError::Unavailable);
            }
            Ok(ControlLogRecoveryPreview {
                authority: preview,
                impact,
            })
        })();
        let _ = self.writer.detach(successor);
        result
    }

    /// Resumes the same reviewed operation, including an interrupted header initialization.
    /// New commands become admissible only after the authority's Activated fact is durable.
    pub fn seal_and_rotate_control_log(
        &self,
        preview: &ControlLogRecoveryPreview,
    ) -> Result<CommandJournalBinding, ApplicationError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if preview.request.logical_journal_id != self.logical_journal_id {
            return Err(ApplicationError::ScopeMismatch);
        }
        if impact_digest(&preview.impact)? != preview.request.owner_context_digest {
            return Err(ApplicationError::ScopeMismatch);
        }
        let recovery = self
            .recovery_lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let recovery = recovery.as_ref().ok_or(ApplicationError::Unavailable)?;
        if let Some(state) = self
            .writer
            .control_log_recovery_state(recovery)
            .map_err(|_| ApplicationError::Unavailable)?
            && state.phase == ControlLogRecoveryPhaseV1::Activated
            && state.preview == preview.authority
            && self.generation.load(Ordering::Acquire) == preview.request.successor_generation
        {
            return Ok(self.binding());
        }
        if preview.request.from_generation != self.generation.load(Ordering::Acquire) {
            return Err(ApplicationError::ScopeMismatch);
        }
        let (impact, length, digest) = self.recovery_impact(&mut entries)?;
        if impact != preview.impact
            || length != preview.old_byte_length
            || digest != preview.old_content_digest
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        let mut old_slot = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let old = old_slot.as_ref().ok_or(ApplicationError::Unavailable)?;
        let successor_key = generation_key(&self.logical_key, preview.request.successor_generation);
        let successor = PendingNamespace::acquire(
            &self.writer,
            StorageWriterChannelV1::ApplicationControlLog,
            &successor_key,
        )?;
        let header = self.header(
            preview.request.operation_id.clone(),
            preview.request.successor_generation,
            preview.old_namespace_hash.to_hex(),
            preview.old_byte_length,
            preview.old_content_digest.to_hex(),
        )?;
        let outcome = self.writer.advance_control_log_recovery(
            recovery,
            old,
            successor.get()?,
            preview,
            &header,
        );
        let state = match outcome {
            Ok(state) => state,
            Err(_) => {
                return Err(ApplicationError::Unavailable);
            }
        };
        if state.phase != ControlLogRecoveryPhaseV1::Activated {
            return Err(ApplicationError::Unavailable);
        }
        let new_index = PendingNamespace::acquire(
            &self.writer,
            StorageWriterChannelV1::ApplicationCommandIndex,
            &successor_key,
        )?;
        let binding = CommandJournalBinding {
            logical_journal_id: self.logical_journal_id.to_hex(),
            command_generation: preview.request.successor_generation,
        };
        let new_entries = match ReservationIndex::open(
            &self.writer,
            successor.get()?,
            new_index.get()?,
            binding.clone(),
            false,
        ) {
            Ok(index) => index,
            Err(error) => {
                return Err(error);
            }
        };
        entries.close();
        let mut index_slot = self
            .index_lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if let Some(old_index) = index_slot.replace(new_index.take()?) {
            let _ = self.writer.detach(old_index);
        }
        if let Some(old) = old_slot.replace(successor.take()?) {
            let _ = self.writer.detach(old);
        }
        *entries = new_entries;
        self.generation
            .store(binding.command_generation, Ordering::Release);
        Ok(binding)
    }

    pub(super) fn historical_record(
        &self,
        generation: u64,
        key: &CommandReservationKey,
    ) -> Result<Option<ReservationRecord>, ApplicationError> {
        let epoch_key = generation_key(&self.logical_key, generation);
        let source = PendingNamespace::acquire(
            &self.writer,
            StorageWriterChannelV1::ApplicationControlLog,
            &epoch_key,
        )?;
        let index = PendingNamespace::acquire(
            &self.writer,
            StorageWriterChannelV1::ApplicationCommandIndex,
            &epoch_key,
        )?;
        let binding = CommandJournalBinding {
            logical_journal_id: self.logical_journal_id.to_hex(),
            command_generation: generation,
        };
        (|| {
            let index =
                ReservationIndex::open(&self.writer, source.get()?, index.get()?, binding, true)?;
            index.get(key)
        })()
    }
}

fn impact_digest(
    impact: &ControlLogRecoveryImpact,
) -> Result<sigil_kernel::resource::CanonicalHash, ApplicationError> {
    let bytes = serde_json::to_vec(impact).map_err(|_| ApplicationError::Unavailable)?;
    Ok(sigil_kernel::resource::CanonicalHash::from_bytes(
        Sha256::digest(bytes).into(),
    ))
}
