//! Atomic, non-overwriting control-log recovery owned by Resource Authority.
use std::io::Write;

use sigil_kernel::managed_storage::{
    ControlLogRecoveryPhaseV1 as Phase, ControlLogRecoveryPreviewV1 as Preview,
    ControlLogRecoveryRequestV1 as Request, ControlLogRecoveryStateV1 as State,
    ManagedStorageForwardGuardV1,
};

use super::*;

const SELECTED: &str = ".control-log-recovery-selected.json";
const INITIALIZING: &str = ".control-log-initializing.json";
const UNINITIALIZED: &str = ".control-log-uninitialized.json";
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const PHASES: [Phase; 4] = [
    Phase::Prepared,
    Phase::Sealed,
    Phase::HeaderInitialized,
    Phase::Activated,
];

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Selection {
    recovery_namespace_hash: CanonicalHash,
    state: State,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct UninitializedNamespace {
    namespace_hash: CanonicalHash,
}

struct ForwardGuard {
    // Field order releases the physical lock before the live namespace and process ownership.
    _lock: File,
    _record: StorageAdmissionRecordV1,
    _process_state: Option<Arc<StorageProcessStateV1>>,
}
impl ManagedStorageForwardGuardV1 for ForwardGuard {}

struct Chain {
    latest: Option<State>,
    bytes: u64,
    entries: u64,
}

impl AuthorityManagedStorageServiceV1 {
    fn recovery_directory(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        owner: ManagedStorageSemanticOwnerV1,
    ) -> Result<PathBuf, ManagedStorageErrorV1> {
        let record = self.record_for_handle(handle)?;
        if record.grant.semantic_owner != owner {
            return Err(ManagedStorageErrorV1::FamilyMismatch);
        }
        let directory = self.physical_namespace_directory(&record)?;
        self.validate_existing_namespace_marker(
            owner,
            handle.namespace_hash,
            &ManagedStorageExistingNamespaceBindingV1 {
                original_handle_id: handle.handle_id.clone(),
                original_namespace_hash: handle.namespace_hash,
            },
        )?;
        Ok(directory)
    }

    pub(super) fn control_forward_guard(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<Box<dyn ManagedStorageForwardGuardV1>, ManagedStorageErrorV1> {
        let record = self.record_for_handle(handle)?;
        let directory =
            self.recovery_directory(handle, ManagedStorageSemanticOwnerV1::ApplicationControlLog)?;
        let lock = open_physical_namespace_lock(&directory)?;
        self.validate_namespace_write(handle)?;
        Ok(Box::new(ForwardGuard {
            _lock: lock,
            _record: record,
            _process_state: self._process_state.clone(),
        }))
    }

    /// Called under the same physical namespace lock as every append/dispatch.
    pub(super) fn validate_control_log_fence(
        &self,
        record: &StorageAdmissionRecordV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        if record.grant.semantic_owner != ManagedStorageSemanticOwnerV1::ApplicationControlLog
            || self.state_root.is_none()
        {
            return Ok(());
        }
        let directory = self.physical_namespace_directory(record)?;
        if metadata_exists(&directory.join(SELECTED))? {
            // A durable selection already freezes old writes; sealing does not depend on an
            // append to the damaged log, nor on in-memory writer ownership.
            let selected: Selection = read_metadata(&directory.join(SELECTED))?;
            validate_state(&selected.state)?;
            if selected.state.preview.old_namespace_hash != record.namespace_hash {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        if metadata_exists(&directory.join(INITIALIZING))? {
            let selected: Selection = read_metadata(&directory.join(INITIALIZING))?;
            validate_state(&selected.state)?;
            if selected.state.preview.successor_namespace_hash != record.namespace_hash {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            let registry = self.physical_namespace_directory_for(
                ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
                selected.recovery_namespace_hash,
            )?;
            let chain = self.read_recovery_chain(&registry, selected.recovery_namespace_hash)?;
            if !chain.latest.as_ref().is_some_and(|state| {
                state.phase == Phase::Activated && state.preview == selected.state.preview
            }) {
                return Err(ManagedStorageErrorV1::HandleFinalized);
            }
            self.sync_observed_recovery_state(&registry, chain.latest.as_ref())?;
        } else if metadata_exists(&directory.join(UNINITIALIZED))? {
            let pending: UninitializedNamespace = read_metadata(&directory.join(UNINITIALIZED))?;
            if pending.namespace_hash != record.namespace_hash {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        Ok(())
    }

    pub(super) fn control_recovery_query(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
    ) -> Result<Option<State>, ManagedStorageErrorV1> {
        let registry = self.recovery_directory(
            recovery,
            ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        )?;
        let _lock = open_physical_namespace_lock(&registry)?;
        let chain = self.read_recovery_chain(&registry, recovery.namespace_hash)?;
        self.sync_observed_recovery_state(&registry, chain.latest.as_ref())?;
        // Cold authority open retires prior process reservations. Reconstruct the physical
        // registry charge before exposing its current generation to another live holder.
        self.reserve_namespace_quota_capacity(recovery, chain.bytes, chain.bytes, chain.entries)?;
        Ok(chain.latest)
    }

    fn sync_observed_recovery_state(
        &self,
        registry: &Path,
        state: Option<&State>,
    ) -> Result<(), ManagedStorageErrorV1> {
        if let Some(state) = state {
            let canonical = registry.join(phase_name(
                state.preview.request.from_generation,
                state.phase,
            ));
            let path = if metadata_exists(&canonical)? {
                canonical
            } else if state.phase == Phase::Prepared {
                registry.join(selection_stage(state.preview.request.from_generation))
            } else {
                return Err(ManagedStorageErrorV1::AuthorityUnavailable);
            };
            // A prior publish may have become visible before its directory sync returned an
            // error. Do not turn that visibility into a durable Activated admission fact.
            open_no_follow_file(&path)
                .map_err(io_error)?
                .sync_all()
                .map_err(io_error)?;
            if state.phase == Phase::Activated {
                fail_activated_directory_sync()?;
            }
            sync_directory_chain(
                registry,
                self.state_root
                    .as_deref()
                    .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
            )?;
        }
        Ok(())
    }

    pub(super) fn control_recovery_frontier(
        &self,
        record: &StorageAdmissionRecordV1,
        registry: &Path,
    ) -> Result<PhysicalStorageFrontierV1, ManagedStorageErrorV1> {
        let chain = self.read_recovery_chain(registry, record.namespace_hash)?;
        Ok(PhysicalStorageFrontierV1 {
            byte_length: chain.bytes,
            record_count: chain.entries,
            content_hash: hash_canonical(&(
                "control-recovery-physical-chain-v1",
                chain.latest.as_ref().map(|state| state.phase_digest),
                chain.bytes,
                chain.entries,
            )),
        })
    }

    pub(super) fn control_recovery_preview(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
        old: &ManagedStorageNamespaceHandleV1,
        successor: &ManagedStorageNamespaceHandleV1,
        request: Request,
    ) -> Result<Preview, ManagedStorageErrorV1> {
        validate_request(&request)?;
        let registry = self.recovery_directory(
            recovery,
            ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        )?;
        let old_directory =
            self.recovery_directory(old, ManagedStorageSemanticOwnerV1::ApplicationControlLog)?;
        let new_directory = self.recovery_directory(
            successor,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        )?;
        if old.namespace_hash == successor.namespace_hash {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let _registry_lock = open_physical_namespace_lock(&registry)?;
        let _old_lock = open_physical_namespace_lock(&old_directory)?;
        let chain = self.read_recovery_chain(&registry, recovery.namespace_hash)?;
        if let Some(state) = chain.latest.as_ref()
            && state.preview.request.from_generation == request.from_generation
        {
            return if state.preview.request == request
                && state.preview.old_namespace_hash == old.namespace_hash
                && state.preview.successor_namespace_hash == successor.namespace_hash
            {
                Ok(state.preview.clone())
            } else {
                Err(ManagedStorageErrorV1::CapabilityMismatch)
            };
        }
        validate_next_request(chain.latest.as_ref(), &request, old.namespace_hash)?;
        if metadata_exists(&old_directory.join(SELECTED))? {
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        let _new_lock = open_physical_namespace_lock(&new_directory)?;
        if metadata_exists(&new_directory.join("records.jsonl"))?
            || metadata_exists(&new_directory.join(INITIALIZING))?
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        // An allocated successor is not an active journal. This operation-neutral allocation
        // marker fences normal appends before confirm commits the selected recovery operation.
        publish_exact(
            &new_directory.join(UNINITIALIZED),
            &encode(&UninitializedNamespace {
                namespace_hash: successor.namespace_hash,
            })?,
            false,
        )?;
        sync_directory_chain(
            &new_directory,
            self.state_root
                .as_deref()
                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
        )?;
        let (old_byte_length, old_content_digest) = physical_digest(&old_directory)?;
        let mut preview = Preview {
            request,
            old_namespace_hash: old.namespace_hash,
            successor_namespace_hash: successor.namespace_hash,
            old_byte_length,
            old_content_digest,
            old_file_identity: physical_identity(&old_directory)?,
            preview_digest: zero_hash(),
        };
        preview.preview_digest = preview_digest(&preview);
        Ok(preview)
    }

    pub(super) fn control_recovery_advance(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
        old: &ManagedStorageNamespaceHandleV1,
        successor: &ManagedStorageNamespaceHandleV1,
        preview: &Preview,
        header: &[u8],
    ) -> Result<State, ManagedStorageErrorV1> {
        validate_preview(preview)?;
        if old.namespace_hash != preview.old_namespace_hash
            || successor.namespace_hash != preview.successor_namespace_hash
            || old.namespace_hash == successor.namespace_hash
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let registry = self.recovery_directory(
            recovery,
            ManagedStorageSemanticOwnerV1::ApplicationControlRecovery,
        )?;
        let old_directory =
            self.recovery_directory(old, ManagedStorageSemanticOwnerV1::ApplicationControlLog)?;
        let new_directory = self.recovery_directory(
            successor,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog,
        )?;
        let _registry_lock = open_physical_namespace_lock(&registry)?;
        let _old_lock = open_physical_namespace_lock(&old_directory)?;
        let _new_lock = open_physical_namespace_lock(&new_directory)?;
        let chain = self.read_recovery_chain(&registry, recovery.namespace_hash)?;
        let mut previous_digest = chain
            .latest
            .as_ref()
            .map_or(zero_hash(), |state| state.phase_digest);
        let mut current_phase = None;
        if let Some(state) = chain.latest.as_ref() {
            if state.preview.request.from_generation == preview.request.from_generation {
                if &state.preview != preview {
                    return Err(ManagedStorageErrorV1::CapabilityMismatch);
                }
                if state.phase == Phase::Activated {
                    // Activated retries never initialize, overwrite, inspect the business tail or
                    // replace a header, even after subsequent records or damage have appeared.
                    open_no_follow_file(&registry.join(phase_name(
                        preview.request.from_generation,
                        Phase::Activated,
                    )))
                    .map_err(io_error)?
                    .sync_all()
                    .map_err(io_error)?;
                    sync_directory_chain(
                        &registry,
                        self.state_root
                            .as_deref()
                            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
                    )?;
                    return Ok(state.clone());
                }
                current_phase = Some(state.phase);
            } else {
                validate_next_request(Some(state), &preview.request, old.namespace_hash)?;
            }
        } else {
            validate_next_request(None, &preview.request, old.namespace_hash)?;
        }
        validate_header(preview, header)?;
        if physical_digest(&old_directory)? != (preview.old_byte_length, preview.old_content_digest)
            || physical_identity(&old_directory)? != preview.old_file_identity
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let phase_bytes_budget = chain.bytes.saturating_add(
            encode(&make_state(preview, Phase::Prepared, previous_digest))?.len() as u64 * 7,
        );
        self.reserve_namespace_quota_capacity(
            recovery,
            phase_bytes_budget,
            phase_bytes_budget,
            chain.entries.saturating_add(5),
        )?;
        self.reserve_namespace_quota_capacity(
            successor,
            header.len() as u64,
            header.len() as u64,
            1,
        )?;
        if current_phase.is_none() {
            let prepared = make_state(preview, Phase::Prepared, previous_digest);
            let prepared_bytes = encode(&prepared)?;
            // This private staging record is durable before the global old-generation selection.
            // Query can recover the selected decision if publication of its registry link fails.
            write_staging(
                &registry.join(selection_stage(preview.request.from_generation)),
                &prepared_bytes,
            )?;
            sync_directory_chain(
                &registry,
                self.state_root
                    .as_deref()
                    .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
            )?;
            let selection = Selection {
                recovery_namespace_hash: recovery.namespace_hash,
                state: prepared.clone(),
            };
            publish_exact(&old_directory.join(SELECTED), &encode(&selection)?, false)?;
            sync_directory_chain(
                &old_directory,
                self.state_root
                    .as_deref()
                    .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
            )?;
            fail_selected_publication()?;
            publish_exact(
                &registry.join(phase_name(preview.request.from_generation, Phase::Prepared)),
                &prepared_bytes,
                false,
            )?;
            remove_staging(&registry.join(selection_stage(preview.request.from_generation)))?;
            previous_digest = prepared.phase_digest;
            current_phase = Some(Phase::Prepared);
            fail_after(Phase::Prepared)?;
        }
        // A selected Prepared recovered from its staging record may not have the registry link.
        if current_phase == Some(Phase::Prepared) {
            let prepared = chain
                .latest
                .as_ref()
                .filter(|state| state.preview == *preview && state.phase == Phase::Prepared)
                .cloned()
                .unwrap_or_else(|| {
                    make_state(
                        preview,
                        Phase::Prepared,
                        chain
                            .latest
                            .as_ref()
                            .map_or(zero_hash(), |state| state.phase_digest),
                    )
                });
            publish_exact(
                &registry.join(phase_name(preview.request.from_generation, Phase::Prepared)),
                &encode(&prepared)?,
                false,
            )?;
            remove_staging(&registry.join(selection_stage(preview.request.from_generation)))?;
            let selection: Selection = read_metadata(&old_directory.join(SELECTED))?;
            if selection.recovery_namespace_hash != recovery.namespace_hash
                || selection.state != prepared
            {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            let sealed = make_state(preview, Phase::Sealed, previous_digest);
            publish_exact(
                &registry.join(phase_name(preview.request.from_generation, Phase::Sealed)),
                &encode(&sealed)?,
                false,
            )?;
            previous_digest = sealed.phase_digest;
            current_phase = Some(Phase::Sealed);
            fail_after(Phase::Sealed)?;
        }
        if matches!(
            current_phase,
            Some(Phase::Sealed | Phase::HeaderInitialized)
        ) {
            let selection = Selection {
                recovery_namespace_hash: recovery.namespace_hash,
                state: make_state(
                    preview,
                    Phase::Sealed,
                    read_metadata::<State>(
                        &registry
                            .join(phase_name(preview.request.from_generation, Phase::Prepared)),
                    )?
                    .phase_digest,
                ),
            };
            publish_exact(
                &new_directory.join(INITIALIZING),
                &encode(&selection)?,
                false,
            )?;
            // Only an exact, header-only canonical file is reusable before activation.
            publish_exact(&new_directory.join("records.jsonl"), header, true)?;
            let marker = open_no_follow_file(&new_directory.join("authority-admission.json"))
                .map_err(io_error)?;
            fail_header_marker_sync()?;
            marker.sync_all().map_err(io_error)?;
            fail_header_parent_sync()?;
            sync_directory_chain(
                &new_directory,
                self.state_root
                    .as_deref()
                    .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
            )?;
            if current_phase == Some(Phase::Sealed) {
                let initialized = make_state(preview, Phase::HeaderInitialized, previous_digest);
                publish_exact(
                    &registry.join(phase_name(
                        preview.request.from_generation,
                        Phase::HeaderInitialized,
                    )),
                    &encode(&initialized)?,
                    false,
                )?;
                previous_digest = initialized.phase_digest;
                fail_after(Phase::HeaderInitialized)?;
            }
        }
        let activated = make_state(preview, Phase::Activated, previous_digest);
        publish_exact(
            &registry.join(phase_name(
                preview.request.from_generation,
                Phase::Activated,
            )),
            &encode(&activated)?,
            false,
        )?;
        sync_directory_chain(
            &registry,
            self.state_root
                .as_deref()
                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
        )?;
        fail_after(Phase::Activated)?;
        Ok(activated)
    }

    fn read_recovery_chain(
        &self,
        registry: &Path,
        recovery_hash: CanonicalHash,
    ) -> Result<Chain, ManagedStorageErrorV1> {
        let mut chain = Chain {
            latest: None,
            bytes: 0,
            entries: 0,
        };
        let mut generation = 0u64;
        loop {
            for phase in PHASES {
                let path = registry.join(phase_name(generation, phase));
                let state = if metadata_exists(&path)? {
                    let bytes = read_bounded(&path)?;
                    chain.bytes = chain.bytes.saturating_add(bytes.len() as u64);
                    chain.entries += 1;
                    serde_json::from_slice::<State>(&bytes)
                        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
                } else {
                    // Detect holes and foreign phase files rather than silently selecting an old
                    // activated generation. Iterate names with constant memory, no lifetime cap.
                    for entry in fs::read_dir(registry).map_err(io_error)? {
                        let name = entry.map_err(io_error)?.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("recovery-") && name.ends_with(".json") {
                            let suffix = name
                                .strip_prefix("recovery-")
                                .and_then(|value| value.strip_suffix(".json"))
                                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?;
                            let (number, stage) = suffix
                                .split_once('-')
                                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?;
                            if number.len() != 20
                                || !number.bytes().all(|byte| byte.is_ascii_digit())
                                || !matches!(stage, "0" | "1" | "2" | "3")
                                || name.as_ref() >= phase_name(generation, phase).as_str()
                            {
                                return Err(ManagedStorageErrorV1::AuthorityUnavailable);
                            }
                        }
                    }
                    if phase == Phase::Prepared {
                        let stage = registry.join(selection_stage(generation));
                        if metadata_exists(&stage)?
                            && let Ok(staged) = read_metadata::<State>(&stage)
                        {
                            validate_state(&staged)?;
                            let old_directory = self.physical_namespace_directory_for(
                                ManagedStorageSemanticOwnerV1::ApplicationControlLog,
                                staged.preview.old_namespace_hash,
                            )?;
                            if metadata_exists(&old_directory.join(SELECTED))? {
                                let selection: Selection =
                                    read_metadata(&old_directory.join(SELECTED))?;
                                if selection.recovery_namespace_hash != recovery_hash
                                    || selection.state != staged
                                    || staged.phase != Phase::Prepared
                                    || staged.preview.request.from_generation != generation
                                {
                                    return Err(ManagedStorageErrorV1::CapabilityMismatch);
                                }
                                validate_chain_link(chain.latest.as_ref(), &staged)?;
                                chain.bytes = chain.bytes.saturating_add(
                                    fs::symlink_metadata(&stage).map_err(io_error)?.len(),
                                );
                                chain.entries += 1;
                                chain.latest = Some(staged);
                            }
                        }
                    }
                    return Ok(chain);
                };
                if state.phase != phase || state.preview.request.from_generation != generation {
                    return Err(ManagedStorageErrorV1::CapabilityMismatch);
                }
                validate_state(&state)?;
                validate_chain_link(chain.latest.as_ref(), &state)?;
                chain.latest = Some(state);
            }
            generation = generation
                .checked_add(1)
                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?;
        }
    }
}

fn validate_request(request: &Request) -> Result<(), ManagedStorageErrorV1> {
    if request.operation_id.is_empty()
        || request.operation_id.len() > 256
        || request.operation_id.chars().any(char::is_control)
        || request.from_generation.checked_add(1) != Some(request.successor_generation)
        || !hash_is_nonzero(request.logical_journal_id)
        || !hash_is_nonzero(request.header_digest)
    {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}
fn validate_preview(preview: &Preview) -> Result<(), ManagedStorageErrorV1> {
    validate_request(&preview.request)?;
    if preview.preview_digest != preview_digest(preview) {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}
fn preview_digest(preview: &Preview) -> CanonicalHash {
    hash_canonical(&(
        "control-log-recovery-preview-v1",
        &preview.request,
        preview.old_namespace_hash,
        preview.successor_namespace_hash,
        preview.old_byte_length,
        preview.old_content_digest,
        preview.old_file_identity,
    ))
}
fn make_state(preview: &Preview, phase: Phase, previous_phase_digest: CanonicalHash) -> State {
    let mut state = State {
        preview: preview.clone(),
        phase,
        previous_phase_digest,
        phase_digest: zero_hash(),
    };
    state.phase_digest = hash_canonical(&(
        "control-log-recovery-phase-v1",
        &state.preview,
        state.phase,
        state.previous_phase_digest,
    ));
    state
}
fn validate_state(state: &State) -> Result<(), ManagedStorageErrorV1> {
    validate_preview(&state.preview)?;
    if make_state(&state.preview, state.phase, state.previous_phase_digest) != *state {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}
fn validate_next_request(
    previous: Option<&State>,
    request: &Request,
    old_namespace_hash: CanonicalHash,
) -> Result<(), ManagedStorageErrorV1> {
    let valid = match previous {
        None => request.from_generation == 0 && request.logical_journal_id == old_namespace_hash,
        Some(state) => {
            state.phase == Phase::Activated
                && request.from_generation == state.preview.request.successor_generation
                && request.logical_journal_id == state.preview.request.logical_journal_id
                && old_namespace_hash == state.preview.successor_namespace_hash
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    }
}
fn validate_chain_link(
    previous: Option<&State>,
    state: &State,
) -> Result<(), ManagedStorageErrorV1> {
    if state.previous_phase_digest != previous.map_or(zero_hash(), |value| value.phase_digest) {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    if state.phase == Phase::Prepared {
        return validate_next_request(
            previous,
            &state.preview.request,
            state.preview.old_namespace_hash,
        );
    }
    if !previous.is_some_and(|previous| {
        previous.preview == state.preview
            && phase_index(previous.phase) + 1 == phase_index(state.phase)
    }) {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}
fn validate_header(preview: &Preview, header: &[u8]) -> Result<(), ManagedStorageErrorV1> {
    if header.is_empty()
        || header.len() as u64 > MAX_METADATA_BYTES
        || !header.ends_with(b"\n")
        || header[..header.len() - 1].contains(&b'\n')
        || hash_bytes(header) != preview.request.header_digest
    {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    let value: serde_json::Value =
        serde_json::from_slice(header).map_err(|_| ManagedStorageErrorV1::CapabilityMismatch)?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || value.get("type").and_then(serde_json::Value::as_str)
            != Some("application_control_header")
        || value
            .get("logical_journal_id")
            .and_then(serde_json::Value::as_str)
            != Some(preview.request.logical_journal_id.to_hex().as_str())
        || value
            .get("command_generation")
            .and_then(serde_json::Value::as_u64)
            != Some(preview.request.successor_generation)
        || value
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            != Some(preview.request.operation_id.as_str())
        || value
            .get("old_namespace_hash")
            .and_then(serde_json::Value::as_str)
            != Some(preview.old_namespace_hash.to_hex().as_str())
        || value
            .get("old_byte_length")
            .and_then(serde_json::Value::as_u64)
            != Some(preview.old_byte_length)
        || value
            .get("old_content_digest")
            .and_then(serde_json::Value::as_str)
            != Some(preview.old_content_digest.to_hex().as_str())
    {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}
fn phase_index(phase: Phase) -> u8 {
    match phase {
        Phase::Prepared => 0,
        Phase::Sealed => 1,
        Phase::HeaderInitialized => 2,
        Phase::Activated => 3,
    }
}
fn phase_name(generation: u64, phase: Phase) -> String {
    format!("recovery-{generation:020}-{}.json", phase_index(phase))
}
fn selection_stage(generation: u64) -> String {
    format!(".recovery-selection-{generation:020}.staged")
}
fn zero_hash() -> CanonicalHash {
    CanonicalHash::from_bytes([0; 32])
}
fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ManagedStorageErrorV1> {
    serde_json::to_vec(value).map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)
}
fn io_error(_: std::io::Error) -> ManagedStorageErrorV1 {
    ManagedStorageErrorV1::AuthorityUnavailable
}
fn metadata_exists(path: &Path) -> Result<bool, ManagedStorageErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && is_safe_physical_metadata(&metadata) => Ok(true),
        Ok(_) => Err(ManagedStorageErrorV1::AuthorityUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(error)),
    }
}
fn read_bounded(path: &Path) -> Result<Vec<u8>, ManagedStorageErrorV1> {
    let mut file = open_no_follow_file(path).map_err(io_error)?;
    if file.metadata().map_err(io_error)?.len() > MAX_METADATA_BYTES {
        return Err(ManagedStorageErrorV1::AuthorityUnavailable);
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(ManagedStorageErrorV1::AuthorityUnavailable);
    }
    Ok(bytes)
}
fn read_metadata<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ManagedStorageErrorV1> {
    serde_json::from_slice(&read_bounded(path)?)
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)
}
fn physical_digest(directory: &Path) -> Result<(u64, CanonicalHash), ManagedStorageErrorV1> {
    let path = directory.join("records.jsonl");
    if !metadata_exists(&path)? {
        return Ok((0, hash_bytes(&[])));
    }
    sigil_kernel::managed_storage::read_physical_digest(
        open_no_follow_file(&path).map_err(io_error)?,
    )
    .map_err(io_error)
}
fn physical_identity(directory: &Path) -> Result<Option<CanonicalHash>, ManagedStorageErrorV1> {
    let path = directory.join("records.jsonl");
    if !metadata_exists(&path)? {
        return Ok(None);
    }
    let file = open_no_follow_file(&path).map_err(io_error)?;
    #[cfg(windows)]
    let identity = crate::identity::canonical_identity_from_handle(&path, &file)
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
    #[cfg(not(windows))]
    let identity = crate::identity::canonical_identity_from_metadata(
        &path,
        &file.metadata().map_err(io_error)?,
    );
    if !identity.is_regular_file || identity.is_symlink || identity.link_count > 1 {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(Some(identity.digest))
}
fn write_staging(path: &Path, bytes: &[u8]) -> Result<(), ManagedStorageErrorV1> {
    write_staging_with_sync_fault(path, bytes, false)
}
fn write_staging_with_sync_fault(
    path: &Path,
    bytes: &[u8],
    header: bool,
) -> Result<(), ManagedStorageErrorV1> {
    // Only an unpublished, RA-private staging name can be replaced. Canonical destinations never
    // use truncate/rename-overwrite. Reject symlinks and reparse points even for staging cleanup.
    if metadata_exists(path)? {
        fs::remove_file(path).map_err(io_error)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(io_error)?;
    sigil_kernel::secure_private_path_permissions(path)
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
    file.write_all(bytes).map_err(io_error)?;
    if header {
        fail_header_sync()?;
    }
    file.sync_all().map_err(io_error)
}
fn publish_exact(path: &Path, bytes: &[u8], header: bool) -> Result<(), ManagedStorageErrorV1> {
    if metadata_exists(path)? {
        if read_bounded(path)? != bytes {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        open_no_follow_file(path)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        let stage = path.with_file_name(format!(
            ".{}.staged",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?
        ));
        if metadata_exists(&stage)? && read_bounded(&stage)? == bytes {
            remove_staging(&stage)?;
        }
        return sync_directory(
            path.parent()
                .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
        );
    }
    let stage = path.with_file_name(format!(
        ".{}.staged",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?
    ));
    if header {
        fail_partial_header(&stage, bytes)?;
    }
    write_staging_with_sync_fault(&stage, bytes, header)?;
    match fs::hard_link(&stage, path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_bounded(path)? != bytes {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
        }
        Err(error) => return Err(io_error(error)),
    }
    if header {
        fail_header_directory_sync()?;
    } else if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with("-3.json"))
    {
        fail_activated_directory_sync()?;
    }
    sync_directory(
        path.parent()
            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?,
    )?;
    fs::remove_file(stage).map_err(io_error)?;
    Ok(())
}
fn remove_staging(path: &Path) -> Result<(), ManagedStorageErrorV1> {
    if metadata_exists(path)? {
        fs::remove_file(path).map_err(io_error)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ManagedStorageErrorV1> {
    reject_reparse_components(path, false).map_err(io_error)?;
    #[cfg(unix)]
    {
        File::open(path)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(io_error)?;
        file.sync_all().map_err(io_error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(ManagedStorageErrorV1::AuthorityUnavailable)
    }
}
fn sync_directory_chain(directory: &Path, root: &Path) -> Result<(), ManagedStorageErrorV1> {
    let root = root.canonicalize().map_err(io_error)?;
    if !directory.starts_with(&root) {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    let mut current = directory;
    loop {
        sync_directory(current)?;
        if current == root {
            break;
        }
        current = current
            .parent()
            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?;
    }
    Ok(())
}

#[cfg(not(test))]
fn fail_selected_publication() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}
#[cfg(not(test))]
fn fail_header_parent_sync() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}
#[cfg(not(test))]
fn fail_after(_: Phase) -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}
#[cfg(not(test))]
fn fail_partial_header(_: &Path, _: &[u8]) -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}
#[cfg(not(test))]
fn fail_header_sync() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}
#[cfg(not(test))]
fn fail_header_directory_sync() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}

#[cfg(not(test))]
fn fail_header_marker_sync() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}

#[cfg(not(test))]
fn fail_activated_directory_sync() -> Result<(), ManagedStorageErrorV1> {
    Ok(())
}

#[cfg(test)]
#[path = "tests/control_log_recovery_tests.rs"]
mod tests;
#[cfg(test)]
use tests::{
    fail_activated_directory_sync, fail_after, fail_header_directory_sync, fail_header_marker_sync,
    fail_header_parent_sync, fail_header_sync, fail_partial_header, fail_selected_publication,
};
