use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::{collections::BTreeMap, sync::Mutex};

use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::*;
use crate::SessionId;

/// The only key slot accepted by the initial encrypted continuation payload backend.
pub const PROVIDER_CONTINUATION_SESSION_KEY_SLOT_ID: &str = "session-master-v1";

/// Maximum opaque provider payload size accepted by the local encrypted backend.
pub const MAX_PROVIDER_CONTINUATION_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

const PAYLOAD_STORE_MAGIC: &[u8; 8] = b"SGCPAY01";
const PAYLOAD_STORE_ENVELOPE_VERSION: u8 = 1;
const PAYLOAD_STORE_HEADER_BYTES: usize = PAYLOAD_STORE_MAGIC.len() + 1 + aead::NONCE_LEN;
const PROVIDER_CONTINUATION_KEYRING_SERVICE: &str =
    "io.github.sigil.provider-continuation-payload.v1";

/// Process- and platform-neutral boundary for session encryption-key persistence.
///
/// Implementations must return `None` only when the referenced key is absent. An unavailable,
/// unreadable, malformed, or rejected secure store is an error: callers must fail closed rather
/// than silently creating a different key while reading existing payloads.
pub(crate) trait ProviderContinuationSessionKeyStore: Send + Sync {
    /// Returns the secret key currently stored for this opaque account.
    ///
    /// # Errors
    ///
    /// Returns an error when the secure store cannot be consulted.
    fn load(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;

    /// Persists a newly generated key while the payload store's cross-process lock is held.
    ///
    /// # Errors
    ///
    /// Returns an error when the secure store rejects or cannot persist the key.
    fn store_new(&self, account: &str, key: &[u8]) -> Result<()>;
}

/// System credential-store implementation for continuation session keys.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemProviderContinuationSessionKeyStore;

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
impl ProviderContinuationSessionKeyStore for SystemProviderContinuationSessionKeyStore {
    fn load(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let entry = keyring::Entry::new(PROVIDER_CONTINUATION_KEYRING_SERVICE, account)
            .context("failed to open provider continuation session key entry")?;
        match entry.get_secret() {
            Ok(key) => Ok(Some(Zeroizing::new(key))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("failed to load provider continuation session key"),
        }
    }

    fn store_new(&self, account: &str, key: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(PROVIDER_CONTINUATION_KEYRING_SERVICE, account)
            .context("failed to open provider continuation session key entry")?;
        entry
            .set_secret(key)
            .context("failed to store provider continuation session key")
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
)))]
impl ProviderContinuationSessionKeyStore for SystemProviderContinuationSessionKeyStore {
    fn load(&self, _account: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        bail!("system credential store is unavailable on this platform")
    }

    fn store_new(&self, _account: &str, _key: &[u8]) -> Result<()> {
        bail!("system credential store is unavailable on this platform")
    }
}

/// Result of staging a payload before the caller appends its durable committed manifest event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderContinuationPayloadStageResult {
    Staged,
    ReusedStaged,
    AlreadyFinalized,
}

/// Result of finalizing a previously staged payload after the durable manifest is committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderContinuationPayloadFinalizeResult {
    Finalized,
    AlreadyFinalized,
}

/// Physical presence of one authenticated payload while its session lock is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderContinuationPayloadPresence {
    Missing,
    Staged,
    Finalized,
}

/// Result of removing every local representation of one payload while its session lock is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderContinuationPayloadDeleteResult {
    Deleted,
    AlreadyAbsent,
}

/// Lock-scoped access to one immutable continuation payload manifest.
///
/// The guard deliberately exposes only authenticated stage/finalize/read/delete operations for
/// its one manifest. Coordinators can therefore hold the same cross-process lock across a
/// physical transition and its durable JSONL transition without receiving the encryption key.
pub(crate) struct ProviderContinuationPayloadStoreGuard<'store, 'manifest, K> {
    store: &'store ProviderContinuationPayloadStore<K>,
    manifest: &'manifest ProviderContinuationPayloadLifecycleEntry,
    key: Zeroizing<[u8; 32]>,
}

/// Session-scoped encrypted local store for opaque provider continuation payload bytes.
///
/// The store never serializes payload bytes into JSONL and never falls back to plaintext. Callers
/// must stage bytes, durably append the matching committed manifest, then finalize. Lifecycle
/// recovery and deletion ordering are intentionally owned by K25.12B2B.
pub(crate) struct ProviderContinuationPayloadStore<K = SystemProviderContinuationSessionKeyStore> {
    root: PathBuf,
    session_id: SessionId,
    key_store: K,
}

impl ProviderContinuationPayloadStore<SystemProviderContinuationSessionKeyStore> {
    /// Constructs the default encrypted payload location next to a durable session stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied session identity is malformed.
    pub(crate) fn for_session_path(
        session_path: &Path,
        session_id: impl Into<SessionId>,
    ) -> Result<Self> {
        let session_id = session_id.into();
        let root = default_provider_continuation_payload_root(session_path, &session_id)?;
        Self::new(root, session_id, SystemProviderContinuationSessionKeyStore)
    }
}

impl<K> ProviderContinuationPayloadStore<K>
where
    K: ProviderContinuationSessionKeyStore,
{
    /// Constructs a store rooted at a caller-selected local state directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the session identity is malformed.
    pub(crate) fn new(
        root: impl Into<PathBuf>,
        session_id: impl Into<SessionId>,
        key_store: K,
    ) -> Result<Self> {
        let session_id = session_id.into();
        validate_store_identity("provider continuation session id", &session_id)?;
        Ok(Self {
            root: root.into(),
            session_id,
            key_store,
        })
    }

    /// Returns the durable session scope whose master key owns this store.
    #[must_use]
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Writes an encrypted staged payload without making it visible as finalized.
    ///
    /// A caller must append and sync the matching `Committed` lifecycle event before calling
    /// [`Self::finalize`]. Repeating a stage with the same manifest and bytes is idempotent;
    /// different bytes under the same manifest fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed manifests, missing secure-key access, integrity mismatch,
    /// unsafe local paths, conflicting staged bytes, or failed durable filesystem operations.
    #[cfg(test)]
    pub(crate) fn stage(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<ProviderContinuationPayloadStageResult> {
        self.with_locked_manifest(manifest, true, |guard| guard.stage(payload))
    }

    /// Atomically makes a staged payload visible after its committed manifest is durable.
    ///
    /// # Errors
    ///
    /// Returns an error if the staged payload/key is missing, does not decrypt against the exact
    /// manifest, or the local filesystem cannot durably rename the encrypted file.
    #[cfg(test)]
    pub(crate) fn finalize(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<ProviderContinuationPayloadFinalizeResult> {
        self.with_locked_manifest(manifest, false, |guard| guard.finalize())
    }

    /// Reads a finalized payload only when the secure key and exact manifest remain available.
    ///
    /// The returned buffer zeroizes its bytes on drop. It is intentionally not serializable or
    /// `Debug`-printable.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is only staged, missing, corrupted, key-unavailable, or
    /// bound to a different manifest/session.
    #[cfg(test)]
    pub(crate) fn read_finalized(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<Zeroizing<Vec<u8>>> {
        self.with_locked_manifest(manifest, false, |guard| guard.read_finalized())
    }

    /// Holds the session payload lock while a coordinator performs one durable transition.
    ///
    /// The closure must not call back into another payload-store operation because it already
    /// owns the store's non-reentrant cross-process lock.
    pub(crate) fn with_locked_manifest<T>(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        create_key_if_absent: bool,
        operation: impl FnOnce(&ProviderContinuationPayloadStoreGuard<'_, '_, K>) -> Result<T>,
    ) -> Result<T> {
        self.with_locked_manifest_key_policy(manifest, || Ok(create_key_if_absent), operation)
    }

    /// Holds the payload lock while deciding whether a new session key may be created.
    ///
    /// The key policy is evaluated only after the cross-process lock is held, so a durable
    /// manifest appended by another writer cannot race a replacement-key decision.
    pub(crate) fn with_locked_manifest_key_policy<T>(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        create_key_if_absent: impl FnOnce() -> Result<bool>,
        operation: impl FnOnce(&ProviderContinuationPayloadStoreGuard<'_, '_, K>) -> Result<T>,
    ) -> Result<T> {
        self.validate_manifest(manifest)?;
        self.with_root_lock(|| {
            let create_key_if_absent = create_key_if_absent()?;
            let key = self.session_key(key_slot_for_manifest(manifest)?, create_key_if_absent)?;
            let guard = ProviderContinuationPayloadStoreGuard {
                store: self,
                manifest,
                key,
            };
            operation(&guard)
        })
    }

    /// Deletes only staged files that have no matching durable committed manifest.
    ///
    /// A finalized file is never inferred to be disposable from its hashed name; it must be
    /// removed through the manifest-bound lifecycle coordinator instead.
    pub(crate) fn discard_uncommitted_stages(
        &self,
        committed_payload_ids: &BTreeSet<ProviderContinuationPayloadId>,
    ) -> Result<usize> {
        let committed_stage_names = committed_payload_ids
            .iter()
            .map(|payload_id| format!("{}.stage", sha256_hex(payload_id.as_bytes())))
            .collect::<BTreeSet<_>>();
        self.with_root_lock(|| {
            let mut removed = 0usize;
            for entry in fs::read_dir(&self.root)
                .with_context(|| format!("failed to read {}", self.root.display()))?
            {
                let entry = entry.context("failed to enumerate provider continuation payload")?;
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                if !is_payload_stage_file_name(&file_name)
                    || committed_stage_names.contains(file_name.as_ref())
                {
                    continue;
                }
                let path = entry.path();
                ensure_regular_payload_file(&path)?;
                fs::remove_file(&path).with_context(|| {
                    format!(
                        "failed to remove uncommitted staged continuation payload {}",
                        path.display()
                    )
                })?;
                removed += 1;
            }
            if removed > 0 {
                sync_directory(&self.root)?;
            }
            Ok(removed)
        })
    }

    fn validate_manifest(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<()> {
        manifest.validate_shape()?;
        if manifest.state != ProviderContinuationPayloadLifecycleState::Committed {
            bail!("provider continuation payload store only accepts committed manifests")
        }
        if manifest.byte_size > MAX_PROVIDER_CONTINUATION_PAYLOAD_BYTES {
            bail!("provider continuation payload exceeds the local encrypted store limit")
        }
        let key_slot = key_slot_for_manifest(manifest)?;
        if key_slot != PROVIDER_CONTINUATION_SESSION_KEY_SLOT_ID {
            bail!("provider continuation payload manifest uses an unsupported key slot")
        }
        Ok(())
    }

    fn validate_payload_bytes(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        payload: &[u8],
    ) -> Result<()> {
        if payload.len() as u64 != manifest.byte_size {
            bail!("provider continuation payload size does not match its manifest")
        }
        if let ProviderContinuationPayloadIntegrity::Sha256(expected) = &manifest.integrity
            && sha256_digest(payload) != *expected
        {
            bail!("provider continuation artifact payload hash does not match its manifest")
        }
        Ok(())
    }

    fn with_root_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.ensure_root()?;
        let lock_path = self.root.join(".payload-store.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open {}", lock_path.display()))?;
        harden_payload_file(&lock_path)?;
        lock.lock_exclusive()
            .context("failed to acquire provider continuation payload lock")?;
        let result = operation();
        let unlock =
            FileExt::unlock(&lock).context("failed to release provider continuation payload lock");
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn session_key(&self, key_slot: &str, create_if_absent: bool) -> Result<Zeroizing<[u8; 32]>> {
        let account = keyring_account(&self.session_id, key_slot);
        let key = match self.key_store.load(&account)? {
            Some(key) => key,
            None if create_if_absent => {
                let mut generated = Zeroizing::new([0_u8; 32]);
                SystemRandom::new()
                    .fill(&mut *generated)
                    .map_err(|_| anyhow!("failed to generate provider continuation session key"))?;
                self.key_store.store_new(&account, &generated[..])?;
                Zeroizing::new(generated.to_vec())
            }
            None => bail!("provider continuation session key is unavailable"),
        };
        let key = key.as_slice();
        if key.len() != 32 {
            bail!("provider continuation session key has an invalid length")
        }
        let mut copied = Zeroizing::new([0_u8; 32]);
        copied.copy_from_slice(key);
        Ok(copied)
    }

    fn associated_data(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<Vec<u8>> {
        serde_json::to_vec(&(PAYLOAD_STORE_ENVELOPE_VERSION, &self.session_id, manifest))
            .context("failed to encode provider continuation payload associated data")
    }

    fn stage_path(&self, manifest: &ProviderContinuationPayloadLifecycleEntry) -> Result<PathBuf> {
        self.payload_path(manifest, "stage")
    }

    fn final_path(&self, manifest: &ProviderContinuationPayloadLifecycleEntry) -> Result<PathBuf> {
        self.payload_path(manifest, "payload")
    }

    fn payload_path(
        &self,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        suffix: &str,
    ) -> Result<PathBuf> {
        validate_store_identity("provider continuation payload id", &manifest.payload_id)?;
        Ok(self.root.join(format!(
            "{}.{}",
            sha256_hex(manifest.payload_id.as_bytes()),
            suffix
        )))
    }

    fn assert_existing_payload_matches(
        &self,
        path: &Path,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        key: &[u8; 32],
        expected: &[u8],
    ) -> Result<()> {
        let payload = self.decrypt_existing_payload(path, manifest, key)?;
        self.validate_payload_bytes(manifest, &payload)?;
        if !expected.is_empty() && payload.as_slice() != expected {
            bail!("provider continuation payload conflicts with the existing staged bytes")
        }
        Ok(())
    }

    fn decrypt_existing_payload(
        &self,
        path: &Path,
        manifest: &ProviderContinuationPayloadLifecycleEntry,
        key: &[u8; 32],
    ) -> Result<Zeroizing<Vec<u8>>> {
        ensure_regular_payload_file(path)?;
        let envelope = fs::read(path).with_context(|| {
            format!(
                "failed to read encrypted continuation payload {}",
                path.display()
            )
        })?;
        let aad = self.associated_data(manifest)?;
        open_payload(key, &aad, &envelope).with_context(|| {
            format!(
                "provider continuation payload {} failed authenticated decryption",
                manifest.payload_id
            )
        })
    }

    fn ensure_root(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("failed to create {}", self.root.display()))?;
        let metadata = fs::symlink_metadata(&self.root)
            .with_context(|| format!("failed to inspect {}", self.root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("provider continuation payload root must be a real directory")
        }
        harden_payload_dir(&self.root)
    }
}

impl<K> ProviderContinuationPayloadStoreGuard<'_, '_, K>
where
    K: ProviderContinuationSessionKeyStore,
{
    pub(crate) fn stage(&self, payload: &[u8]) -> Result<ProviderContinuationPayloadStageResult> {
        self.store.validate_payload_bytes(self.manifest, payload)?;
        let stage_path = self.store.stage_path(self.manifest)?;
        let final_path = self.store.final_path(self.manifest)?;
        if payload_path_exists(&final_path)? {
            self.store.assert_existing_payload_matches(
                &final_path,
                self.manifest,
                &self.key,
                payload,
            )?;
            return Ok(ProviderContinuationPayloadStageResult::AlreadyFinalized);
        }
        if payload_path_exists(&stage_path)? {
            self.store.assert_existing_payload_matches(
                &stage_path,
                self.manifest,
                &self.key,
                payload,
            )?;
            return Ok(ProviderContinuationPayloadStageResult::ReusedStaged);
        }
        let aad = self.store.associated_data(self.manifest)?;
        let envelope = seal_payload(&self.key, &aad, payload)?;
        write_new_hardened_file(&stage_path, &envelope)?;
        Ok(ProviderContinuationPayloadStageResult::Staged)
    }

    pub(crate) fn finalize(&self) -> Result<ProviderContinuationPayloadFinalizeResult> {
        let stage_path = self.store.stage_path(self.manifest)?;
        let final_path = self.store.final_path(self.manifest)?;
        if payload_path_exists(&final_path)? {
            self.store.assert_existing_payload_matches(
                &final_path,
                self.manifest,
                &self.key,
                &[],
            )?;
            if payload_path_exists(&stage_path)? {
                self.store.assert_existing_payload_matches(
                    &stage_path,
                    self.manifest,
                    &self.key,
                    &[],
                )?;
                fs::remove_file(&stage_path).with_context(|| {
                    format!(
                        "failed to remove redundant staged continuation payload {}",
                        stage_path.display()
                    )
                })?;
                sync_directory(&self.store.root)?;
            }
            return Ok(ProviderContinuationPayloadFinalizeResult::AlreadyFinalized);
        }
        self.store
            .assert_existing_payload_matches(&stage_path, self.manifest, &self.key, &[])?;
        fs::rename(&stage_path, &final_path).with_context(|| {
            format!(
                "failed to finalize provider continuation payload {}",
                self.manifest.payload_id
            )
        })?;
        harden_payload_file(&final_path)?;
        sync_directory(&self.store.root)?;
        Ok(ProviderContinuationPayloadFinalizeResult::Finalized)
    }

    #[cfg(test)]
    pub(crate) fn read_finalized(&self) -> Result<Zeroizing<Vec<u8>>> {
        let path = self.store.final_path(self.manifest)?;
        let payload = self
            .store
            .decrypt_existing_payload(&path, self.manifest, &self.key)?;
        self.store.validate_payload_bytes(self.manifest, &payload)?;
        Ok(payload)
    }

    pub(crate) fn presence(&self) -> Result<ProviderContinuationPayloadPresence> {
        let final_path = self.store.final_path(self.manifest)?;
        if payload_path_exists(&final_path)? {
            self.store.assert_existing_payload_matches(
                &final_path,
                self.manifest,
                &self.key,
                &[],
            )?;
            return Ok(ProviderContinuationPayloadPresence::Finalized);
        }
        let stage_path = self.store.stage_path(self.manifest)?;
        if payload_path_exists(&stage_path)? {
            self.store.assert_existing_payload_matches(
                &stage_path,
                self.manifest,
                &self.key,
                &[],
            )?;
            return Ok(ProviderContinuationPayloadPresence::Staged);
        }
        Ok(ProviderContinuationPayloadPresence::Missing)
    }

    pub(crate) fn delete(&self) -> Result<ProviderContinuationPayloadDeleteResult> {
        let mut removed = false;
        for path in [
            self.store.stage_path(self.manifest)?,
            self.store.final_path(self.manifest)?,
        ] {
            if !payload_path_exists(&path)? {
                continue;
            }
            self.store
                .assert_existing_payload_matches(&path, self.manifest, &self.key, &[])?;
            fs::remove_file(&path).with_context(|| {
                format!(
                    "failed to delete provider continuation payload {}",
                    self.manifest.payload_id
                )
            })?;
            removed = true;
        }
        if removed {
            sync_directory(&self.store.root)?;
            Ok(ProviderContinuationPayloadDeleteResult::Deleted)
        } else {
            Ok(ProviderContinuationPayloadDeleteResult::AlreadyAbsent)
        }
    }
}

/// Derives the default encrypted continuation-payload directory for one durable session stream.
///
/// # Errors
///
/// Returns an error when the supplied session identity is malformed.
pub(crate) fn default_provider_continuation_payload_root(
    session_path: &Path,
    session_id: &str,
) -> Result<PathBuf> {
    validate_store_identity("provider continuation session id", session_id)?;
    let parent = session_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent
        .join("continuation-payloads")
        .join(sha256_hex(session_id.as_bytes())))
}

fn key_slot_for_manifest(manifest: &ProviderContinuationPayloadLifecycleEntry) -> Result<&str> {
    match &manifest.storage_ref {
        ProviderContinuationPayloadStorageRef::Artifact { .. } => {
            Ok(PROVIDER_CONTINUATION_SESSION_KEY_SLOT_ID)
        }
        ProviderContinuationPayloadStorageRef::SensitiveState { key_slot_id, .. } => {
            validate_store_identity("provider continuation key slot id", key_slot_id)?;
            Ok(key_slot_id)
        }
    }
}

fn keyring_account(session_id: &str, key_slot: &str) -> String {
    sha256_hex(format!("{session_id}\0{key_slot}").as_bytes())
}

fn seal_payload(key: &[u8; 32], aad: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|_| anyhow!("failed to initialize provider continuation encryption key"))?;
    let key = aead::LessSafeKey::new(unbound);
    let mut nonce = [0_u8; aead::NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow!("failed to generate provider continuation payload nonce"))?;
    let mut ciphertext = Zeroizing::new(payload.to_vec());
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad),
        &mut *ciphertext,
    )
    .map_err(|_| anyhow!("failed to encrypt provider continuation payload"))?;
    let mut envelope = Vec::with_capacity(PAYLOAD_STORE_HEADER_BYTES + ciphertext.len());
    envelope.extend_from_slice(PAYLOAD_STORE_MAGIC);
    envelope.push(PAYLOAD_STORE_ENVELOPE_VERSION);
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

fn open_payload(key: &[u8; 32], aad: &[u8], envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if envelope.len() <= PAYLOAD_STORE_HEADER_BYTES
        || &envelope[..PAYLOAD_STORE_MAGIC.len()] != PAYLOAD_STORE_MAGIC
        || envelope[PAYLOAD_STORE_MAGIC.len()] != PAYLOAD_STORE_ENVELOPE_VERSION
    {
        bail!("provider continuation payload envelope is malformed")
    }
    let nonce_start = PAYLOAD_STORE_MAGIC.len() + 1;
    let nonce_end = nonce_start + aead::NONCE_LEN;
    let mut nonce = [0_u8; aead::NONCE_LEN];
    nonce.copy_from_slice(&envelope[nonce_start..nonce_end]);
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|_| anyhow!("failed to initialize provider continuation decryption key"))?;
    let key = aead::LessSafeKey::new(unbound);
    let mut ciphertext = Zeroizing::new(envelope[PAYLOAD_STORE_HEADER_BYTES..].to_vec());
    let plaintext = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut ciphertext,
        )
        .map_err(|_| anyhow!("provider continuation payload authentication failed"))?;
    Ok(Zeroizing::new(plaintext.to_vec()))
}

fn write_new_hardened_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    harden_payload_file(path)?;
    sync_parent(path)
}

fn ensure_regular_payload_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "failed to inspect encrypted continuation payload {}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("provider continuation payload must be a regular file")
    }
    Ok(())
}

fn payload_path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("provider continuation payload must be a regular file")
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect encrypted continuation payload {}",
                path.display()
            )
        }),
    }
}

fn is_payload_stage_file_name(file_name: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".stage") else {
        return false;
    };
    stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("provider continuation payload path has no parent"))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

#[cfg(unix)]
fn harden_payload_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_payload_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_payload_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_payload_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn validate_store_identity(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        bail!("{field} must be non-empty, bounded, and control-free")
    }
    Ok(())
}

fn sha256_digest(payload: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(payload))
}

fn sha256_hex(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct InMemoryProviderContinuationSessionKeyStore {
    keys: Mutex<BTreeMap<String, Vec<u8>>>,
}

#[cfg(test)]
impl ProviderContinuationSessionKeyStore for InMemoryProviderContinuationSessionKeyStore {
    fn load(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let keys = self
            .keys
            .lock()
            .map_err(|_| anyhow!("in-memory provider continuation key store lock is poisoned"))?;
        Ok(keys.get(account).cloned().map(Zeroizing::new))
    }

    fn store_new(&self, account: &str, key: &[u8]) -> Result<()> {
        let mut keys = self
            .keys
            .lock()
            .map_err(|_| anyhow!("in-memory provider continuation key store lock is poisoned"))?;
        if keys.insert(account.to_owned(), key.to_vec()).is_some() {
            bail!("in-memory provider continuation key unexpectedly already exists")
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/provider_continuation_payload_store_tests.rs"]
mod tests;
