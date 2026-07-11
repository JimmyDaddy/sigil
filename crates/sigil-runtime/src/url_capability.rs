use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use sigil_kernel::{
    SecretString, Session, ToolRestartPolicy, UserUrlCapabilityRegistrar,
    UserUrlCapabilityRegistration, WebUrlCapabilityDescriptor, WebUrlProvenanceKind,
    canonical_web_url_persistence_projection,
};

/// Default lifetime of one live URL capability.
pub const DEFAULT_URL_CAPABILITY_TTL: Duration = Duration::from_secs(3_600);
/// Maximum live URL capabilities retained for one session.
pub const DEFAULT_URL_CAPABILITY_CAPACITY: usize = 256;

/// Creates and attaches one process-local URL capability store for a logical session.
///
/// Call this once when a production session is created or loaded. Ownership moves of `Session`
/// preserve the non-serializable attachment; callers that reload the same JSONL session while the
/// process remains alive should transfer the existing attachment instead of calling this again.
pub fn attach_session_url_capability_store(
    session: &mut Session,
) -> Result<Arc<WebUrlCapabilityStore>> {
    let store = Arc::new(WebUrlCapabilityStore::new(session.session_scope_id())?);
    let registrar: Arc<dyn UserUrlCapabilityRegistrar> = store.clone();
    session.try_attach_user_url_capability_registrar(registrar)?;
    Ok(store)
}

/// Secret-bearing, process-local URL capability.
///
/// The raw URL has no serde implementation and is redacted from `Debug`. Callers may only obtain
/// it after an exact session-scoped lookup succeeds.
#[derive(Clone, PartialEq, Eq)]
pub struct WebUrlCapability {
    session_scope_id: String,
    source_id: String,
    durable_entry_id: String,
    raw_canonical_url: SecretString,
    safe_display_url: String,
    restart_policy: ToolRestartPolicy,
    originating_call_id: Option<String>,
    provenance: WebUrlProvenanceKind,
    issued_at_ms: u64,
    expires_at_ms: u64,
    expires_at: Instant,
}

impl WebUrlCapability {
    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn durable_entry_id(&self) -> &str {
        &self.durable_entry_id
    }

    #[must_use]
    pub fn raw_canonical_url(&self) -> &SecretString {
        &self.raw_canonical_url
    }

    #[must_use]
    pub fn safe_display_url(&self) -> &str {
        &self.safe_display_url
    }

    #[must_use]
    pub fn restart_policy(&self) -> ToolRestartPolicy {
        self.restart_policy
    }

    #[must_use]
    pub fn originating_call_id(&self) -> Option<&str> {
        self.originating_call_id.as_deref()
    }

    #[must_use]
    pub fn provenance(&self) -> WebUrlProvenanceKind {
        self.provenance
    }

    #[must_use]
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    #[must_use]
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    #[must_use]
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

impl fmt::Debug for WebUrlCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebUrlCapability")
            .field("session_scope_id", &self.session_scope_id)
            .field("source_id", &self.source_id)
            .field("durable_entry_id", &self.durable_entry_id)
            .field("raw_canonical_url", &"[redacted]")
            .field("safe_display_url", &self.safe_display_url)
            .field("restart_policy", &self.restart_policy)
            .field("originating_call_id", &self.originating_call_id)
            .field("provenance", &self.provenance)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Stable lookup outcomes for a session-local URL capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlCapabilityLookupError {
    NotFound,
    Expired,
    Evicted,
    InterruptedOnRestart,
}

impl fmt::Display for UrlCapabilityLookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "url capability was not found",
            Self::Expired => "url capability has expired",
            Self::Evicted => "url capability was evicted",
            Self::InterruptedOnRestart => "sensitive URL is not replayable after restart",
        })
    }
}

impl Error for UrlCapabilityLookupError {}

impl UrlCapabilityLookupError {
    /// Stable machine code for product routing and tool-result projection.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Expired => "expired",
            Self::Evicted => "evicted",
            Self::InterruptedOnRestart => "sensitive_url_not_replayable",
        }
    }
}

/// Process-local, session-scoped live URL capability store.
///
/// Registrations are staged before the corresponding safe user message is durably appended and
/// become resolvable only after `commit_message`. The store retains at most `capacity` live
/// entries and the same number of typed expiry/eviction tombstones.
pub struct WebUrlCapabilityStore {
    session_scope_id: String,
    ttl: Duration,
    capacity: usize,
    state: Mutex<StoreState>,
}

impl WebUrlCapabilityStore {
    /// Creates a store with the production TTL and per-session capacity.
    ///
    /// # Errors
    ///
    /// `session_scope_id` must be the durable logical session id reused across process recovery,
    /// never a process-local store id. Returns an error when that scope is empty.
    pub fn new(session_scope_id: impl Into<String>) -> Result<Self> {
        Self::with_limits(
            session_scope_id,
            DEFAULT_URL_CAPABILITY_TTL,
            DEFAULT_URL_CAPABILITY_CAPACITY,
        )
    }

    /// Creates a store with explicit limits, primarily for deterministic lifecycle tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the session scope is empty or capacity is zero.
    pub fn with_limits(
        session_scope_id: impl Into<String>,
        ttl: Duration,
        capacity: usize,
    ) -> Result<Self> {
        let session_scope_id = session_scope_id.into();
        if session_scope_id.trim().is_empty() {
            bail!("URL capability session scope id must not be empty");
        }
        if capacity == 0 {
            bail!("URL capability capacity must be greater than zero");
        }
        if Instant::now().checked_add(ttl).is_none() {
            bail!("URL capability TTL exceeds the platform instant range");
        }
        Ok(Self {
            session_scope_id,
            ttl,
            capacity,
            state: Mutex::new(StoreState::default()),
        })
    }

    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Resolves one live capability and refreshes its per-session LRU position.
    ///
    /// A source id from any other session is deliberately indistinguishable from an unknown id.
    pub fn resolve(
        &self,
        session_scope_id: &str,
        source_id: &str,
    ) -> std::result::Result<WebUrlCapability, UrlCapabilityLookupError> {
        if session_scope_id != self.session_scope_id {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        let mut state = self.lock_state();
        if state.closed {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        state.resolve(source_id, Instant::now(), self.capacity)
    }

    /// Resolves a live capability or reports a restart-only interruption proven by durable state.
    ///
    /// Expiry and eviction tombstones take precedence. An empty store never guesses restart
    /// semantics: only an exact-session descriptor with `InterruptOnRestart` can produce
    /// `InterruptedOnRestart`. A replayable URL is restored only from the descriptor's explicit,
    /// validated queryless canonical URL, never from its display string or a digest.
    pub fn resolve_with_durable_descriptor(
        &self,
        descriptor: &WebUrlCapabilityDescriptor,
    ) -> std::result::Result<WebUrlCapability, UrlCapabilityLookupError> {
        if self.lock_state().closed {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        if descriptor.session_scope_id != self.session_scope_id
            || !is_session_local_source_id(&descriptor.source_id)
            || descriptor.validate().is_err()
        {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        match self.resolve(&descriptor.session_scope_id, &descriptor.source_id) {
            Ok(capability) if capability_matches_descriptor(&capability, descriptor) => {
                Ok(capability)
            }
            Ok(_) => Err(UrlCapabilityLookupError::NotFound),
            Err(UrlCapabilityLookupError::NotFound)
                if descriptor.restart_policy == ToolRestartPolicy::InterruptOnRestart =>
            {
                Err(UrlCapabilityLookupError::InterruptedOnRestart)
            }
            Err(UrlCapabilityLookupError::NotFound)
                if descriptor.restart_policy == ToolRestartPolicy::Replayable =>
            {
                self.restore_replayable_descriptor(descriptor)
            }
            outcome => outcome,
        }
    }

    /// Drops every staged, live, and tombstoned capability for this session.
    pub fn close_session(&self) {
        let mut state = self.lock_state();
        *state = StoreState::default();
        state.closed = true;
    }

    #[must_use]
    pub fn active_len(&self) -> usize {
        self.lock_state().active.len()
    }

    #[must_use]
    pub fn staged_len(&self) -> usize {
        self.lock_state()
            .staged_by_message
            .values()
            .map(BTreeMap::len)
            .sum()
    }

    fn lock_state(&self) -> MutexGuard<'_, StoreState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn validate_registration(&self, registration: &UserUrlCapabilityRegistration) -> Result<()> {
        if !is_session_local_source_id(&registration.source_id) {
            bail!("URL capability source id must be session-local");
        }
        if registration.durable_entry_id.trim().is_empty() {
            bail!("URL capability durable entry id must not be empty");
        }
        if registration.safe_display_url.trim().is_empty() {
            bail!("URL capability safe display URL must not be empty");
        }
        if registration.issued_at_ms == 0 || registration.expires_at_ms <= registration.issued_at_ms
        {
            bail!("URL capability expiry must be after its issue time");
        }
        let raw_url = registration.raw_canonical_url.expose_secret();
        let canonical = canonical_web_url_persistence_projection(raw_url)?;
        if canonical.raw_canonical_url.expose_secret() != raw_url {
            bail!("URL capability raw URL must already be canonical");
        }
        if registration.safe_display_url != canonical.safe_display_url
            || registration.restart_policy != canonical.restart_policy
            || registration.replayable_canonical_url != canonical.replayable_canonical_url
        {
            bail!("URL capability safe display does not match the canonical URL projection");
        }
        registration
            .durable_descriptor(self.session_scope_id.clone())
            .validate()?;
        if registration.restart_policy == ToolRestartPolicy::Replayable
            && registration.replayable_canonical_url.as_deref() != Some(raw_url)
        {
            bail!("replayable URL capability material must equal the live canonical URL");
        }
        Ok(())
    }

    fn restore_replayable_descriptor(
        &self,
        descriptor: &WebUrlCapabilityDescriptor,
    ) -> std::result::Result<WebUrlCapability, UrlCapabilityLookupError> {
        let Some(raw_canonical_url) = descriptor.replayable_canonical_url.as_deref() else {
            return Err(UrlCapabilityLookupError::NotFound);
        };
        let now = Instant::now();
        let now_ms = unix_time_ms();
        if descriptor.expires_at_ms <= now_ms {
            return Err(UrlCapabilityLookupError::Expired);
        }
        let remaining = Duration::from_millis(descriptor.expires_at_ms.saturating_sub(now_ms));
        let Some(expires_at) = now.checked_add(self.ttl.min(remaining)) else {
            return Err(UrlCapabilityLookupError::NotFound);
        };
        let capability = WebUrlCapability {
            session_scope_id: self.session_scope_id.clone(),
            source_id: descriptor.source_id.clone(),
            durable_entry_id: descriptor.durable_entry_id.clone(),
            raw_canonical_url: SecretString::new(raw_canonical_url),
            safe_display_url: descriptor.safe_display_url.clone(),
            restart_policy: ToolRestartPolicy::Replayable,
            originating_call_id: descriptor.originating_call_id.clone(),
            provenance: descriptor.provenance,
            issued_at_ms: descriptor.issued_at_ms,
            expires_at_ms: descriptor.expires_at_ms,
            expires_at,
        };
        let mut state = self.lock_state();
        if state.closed {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        state.purge_expired(now, self.capacity);
        if let Some(tombstone) = state.tombstones.get(&descriptor.source_id) {
            return Err(tombstone.reason.as_lookup_error());
        }
        if let Some(active) = state.active.get(&descriptor.source_id) {
            return Ok(active.capability.clone());
        }
        if state.find_staged(&descriptor.source_id).is_some() {
            return Err(UrlCapabilityLookupError::NotFound);
        }
        state
            .committed_by_message
            .entry(descriptor.durable_entry_id.clone())
            .or_default()
            .insert(descriptor.source_id.clone());
        let last_access_sequence = state.next_sequence();
        state.active.insert(
            descriptor.source_id.clone(),
            ActiveCapability {
                capability: capability.clone(),
                last_access_sequence,
            },
        );
        state.enforce_capacity(self.capacity);
        match state.active.get(&descriptor.source_id) {
            Some(active) => Ok(active.capability.clone()),
            None => Err(UrlCapabilityLookupError::Evicted),
        }
    }
}

impl UserUrlCapabilityRegistrar for WebUrlCapabilityStore {
    fn stage(&self, registration: UserUrlCapabilityRegistration) -> Result<()> {
        self.validate_registration(&registration)?;
        let now = Instant::now();
        let now_ms = unix_time_ms();
        if registration.expires_at_ms <= now_ms {
            bail!("URL capability registration is already expired");
        }
        let mut state = self.lock_state();
        if state.closed {
            bail!("URL capability store is closed");
        }
        state.purge_expired(now, self.capacity);
        state.prune_committed();

        if let Some(committed_sources) = state
            .committed_by_message
            .get(&registration.durable_entry_id)
        {
            if committed_sources.contains(&registration.source_id) {
                return Ok(());
            }
            bail!("URL capability cannot be added after its durable message was committed");
        }
        if let Some(active) = state.active.get(&registration.source_id) {
            if active.capability.durable_entry_id == registration.durable_entry_id
                && registration_matches_capability(&registration, &active.capability)
            {
                return Ok(());
            }
            bail!("URL capability source id is already bound to another registration");
        }
        if let Some(tombstone) = state.tombstones.get(&registration.source_id) {
            if matches!(tombstone.reason, UrlCapabilityTombstone::RolledBack) {
                state.tombstones.remove(&registration.source_id);
            } else {
                bail!("URL capability source id has already completed its live lifecycle");
            }
        }
        if let Some((message_id, staged)) = state.find_staged(&registration.source_id) {
            if message_id == registration.durable_entry_id
                && registration_matches_registration(&registration, staged)
            {
                return Ok(());
            }
            bail!("URL capability source id is already staged by another registration");
        }

        state.reserve_stage_capacity(self.capacity)?;

        let message_id = registration.durable_entry_id.clone();
        let remaining = Duration::from_millis(registration.expires_at_ms.saturating_sub(now_ms));
        let expires_at = now.checked_add(self.ttl.min(remaining)).ok_or_else(|| {
            anyhow::anyhow!("URL capability TTL exceeds the platform instant range")
        })?;
        state
            .staged_by_message
            .entry(message_id)
            .or_default()
            .insert(
                registration.source_id.clone(),
                StagedCapability {
                    registration,
                    expires_at,
                },
            );
        Ok(())
    }

    fn commit_message(&self, durable_entry_id: &str) -> Result<()> {
        if durable_entry_id.trim().is_empty() {
            bail!("URL capability durable entry id must not be empty");
        }
        let now = Instant::now();
        let mut state = self.lock_state();
        if state.closed {
            bail!("URL capability store is closed");
        }
        state.purge_expired(now, self.capacity);
        state.prune_committed();
        if state.committed_by_message.contains_key(durable_entry_id) {
            return Ok(());
        }

        let staged = state
            .staged_by_message
            .remove(durable_entry_id)
            .unwrap_or_default();
        if staged.is_empty() {
            return Ok(());
        }
        let mut committed_sources = BTreeSet::new();
        for (source_id, staged) in staged {
            committed_sources.insert(source_id.clone());
            if staged.expires_at <= now {
                state.record_tombstone(source_id, UrlCapabilityTombstone::Expired, self.capacity);
                continue;
            }
            let registration = staged.registration;
            let capability = WebUrlCapability {
                session_scope_id: self.session_scope_id.clone(),
                source_id: source_id.clone(),
                durable_entry_id: registration.durable_entry_id,
                raw_canonical_url: registration.raw_canonical_url,
                safe_display_url: registration.safe_display_url,
                restart_policy: registration.restart_policy,
                originating_call_id: registration.originating_call_id,
                provenance: registration.provenance,
                issued_at_ms: registration.issued_at_ms,
                expires_at_ms: registration.expires_at_ms,
                expires_at: staged.expires_at,
            };
            let last_access_sequence = state.next_sequence();
            state.active.insert(
                source_id,
                ActiveCapability {
                    capability,
                    last_access_sequence,
                },
            );
        }
        state
            .committed_by_message
            .insert(durable_entry_id.to_owned(), committed_sources);
        state.enforce_capacity(self.capacity);
        state.prune_committed();
        Ok(())
    }

    fn rollback_message(&self, durable_entry_id: &str) -> Result<()> {
        if durable_entry_id.trim().is_empty() {
            bail!("URL capability durable entry id must not be empty");
        }
        let mut state = self.lock_state();
        if state.closed {
            bail!("URL capability store is closed");
        }
        if !state.committed_by_message.contains_key(durable_entry_id) {
            let rolled_back = state
                .staged_by_message
                .remove(durable_entry_id)
                .unwrap_or_default();
            for source_id in rolled_back.into_keys() {
                state.record_tombstone(
                    source_id,
                    UrlCapabilityTombstone::RolledBack,
                    self.capacity,
                );
            }
        }
        Ok(())
    }
}

impl fmt::Debug for WebUrlCapabilityStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock_state();
        formatter
            .debug_struct("WebUrlCapabilityStore")
            .field("session_scope_id", &self.session_scope_id)
            .field("ttl", &self.ttl)
            .field("capacity", &self.capacity)
            .field("staged_message_count", &state.staged_by_message.len())
            .field("active_count", &state.active.len())
            .field("tombstone_count", &state.tombstones.len())
            .finish()
    }
}

#[derive(Default)]
struct StoreState {
    staged_by_message: HashMap<String, BTreeMap<String, StagedCapability>>,
    committed_by_message: HashMap<String, BTreeSet<String>>,
    active: HashMap<String, ActiveCapability>,
    tombstones: HashMap<String, TombstoneEntry>,
    sequence: u64,
    closed: bool,
}

impl StoreState {
    fn reserve_stage_capacity(&mut self, capacity: usize) -> Result<()> {
        let staged = self
            .staged_by_message
            .values()
            .map(BTreeMap::len)
            .sum::<usize>();
        if staged >= capacity {
            // Staging precedes the durable safe-message append. Evicting an existing live
            // capability here would make a later append failure destructive even after rollback.
            // Keep a separately bounded in-flight allowance and defer live LRU eviction until the
            // corresponding safe durable message has committed atomically.
            bail!("URL capability in-flight stage capacity is exhausted");
        }
        Ok(())
    }

    fn resolve(
        &mut self,
        source_id: &str,
        now: Instant,
        tombstone_capacity: usize,
    ) -> std::result::Result<WebUrlCapability, UrlCapabilityLookupError> {
        if let Some(tombstone) = self.tombstones.get(source_id) {
            return Err(tombstone.reason.as_lookup_error());
        }
        if self
            .active
            .get(source_id)
            .is_some_and(|entry| entry.capability.expires_at <= now)
        {
            self.active.remove(source_id);
            self.record_tombstone(
                source_id.to_owned(),
                UrlCapabilityTombstone::Expired,
                tombstone_capacity,
            );
            return Err(UrlCapabilityLookupError::Expired);
        }
        let sequence = self.next_sequence();
        let Some(active) = self.active.get_mut(source_id) else {
            return Err(UrlCapabilityLookupError::NotFound);
        };
        active.last_access_sequence = sequence;
        Ok(active.capability.clone())
    }

    fn find_staged(&self, source_id: &str) -> Option<(&str, &UserUrlCapabilityRegistration)> {
        self.staged_by_message
            .iter()
            .find_map(|(message_id, registrations)| {
                registrations
                    .get(source_id)
                    .map(|staged| (message_id.as_str(), &staged.registration))
            })
    }

    fn purge_expired(&mut self, now: Instant, tombstone_capacity: usize) {
        let expired_staged = self
            .staged_by_message
            .iter_mut()
            .flat_map(|(_, registrations)| {
                let expired = registrations
                    .iter()
                    .filter(|(_, staged)| staged.expires_at <= now)
                    .map(|(source_id, _)| source_id.clone())
                    .collect::<Vec<_>>();
                for source_id in &expired {
                    registrations.remove(source_id);
                }
                expired
            })
            .collect::<Vec<_>>();
        self.staged_by_message
            .retain(|_, registrations| !registrations.is_empty());
        for source_id in expired_staged {
            self.record_tombstone(
                source_id,
                UrlCapabilityTombstone::Expired,
                tombstone_capacity,
            );
        }
        let expired = self
            .active
            .iter()
            .filter(|(_, entry)| entry.capability.expires_at <= now)
            .map(|(source_id, _)| source_id.clone())
            .collect::<Vec<_>>();
        for source_id in expired {
            self.active.remove(&source_id);
            self.record_tombstone(
                source_id,
                UrlCapabilityTombstone::Expired,
                tombstone_capacity,
            );
        }
    }

    fn prune_committed(&mut self) {
        let active = &self.active;
        let tombstones = &self.tombstones;
        self.committed_by_message.retain(|_, source_ids| {
            source_ids.retain(|source_id| {
                active.contains_key(source_id) || tombstones.contains_key(source_id)
            });
            !source_ids.is_empty()
        });
    }

    fn enforce_capacity(&mut self, capacity: usize) {
        while self.active.len() > capacity {
            let Some(source_id) = self
                .active
                .iter()
                .min_by_key(|(_, entry)| entry.last_access_sequence)
                .map(|(source_id, _)| source_id.clone())
            else {
                break;
            };
            self.active.remove(&source_id);
            self.record_tombstone(source_id, UrlCapabilityTombstone::Evicted, capacity);
        }
    }

    fn record_tombstone(
        &mut self,
        source_id: String,
        reason: UrlCapabilityTombstone,
        capacity: usize,
    ) {
        let sequence = self.next_sequence();
        self.tombstones
            .insert(source_id, TombstoneEntry { reason, sequence });
        while self.tombstones.len() > capacity {
            let Some(oldest) = self
                .tombstones
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(source_id, _)| source_id.clone())
            else {
                break;
            };
            self.tombstones.remove(&oldest);
        }
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

struct StagedCapability {
    registration: UserUrlCapabilityRegistration,
    expires_at: Instant,
}

struct ActiveCapability {
    capability: WebUrlCapability,
    last_access_sequence: u64,
}

#[derive(Clone, Copy)]
enum UrlCapabilityTombstone {
    Expired,
    Evicted,
    RolledBack,
}

impl UrlCapabilityTombstone {
    fn as_lookup_error(self) -> UrlCapabilityLookupError {
        match self {
            Self::Expired => UrlCapabilityLookupError::Expired,
            Self::Evicted => UrlCapabilityLookupError::Evicted,
            Self::RolledBack => UrlCapabilityLookupError::NotFound,
        }
    }
}

struct TombstoneEntry {
    reason: UrlCapabilityTombstone,
    sequence: u64,
}

fn registration_matches_registration(
    left: &UserUrlCapabilityRegistration,
    right: &UserUrlCapabilityRegistration,
) -> bool {
    left.source_id == right.source_id
        && left.durable_entry_id == right.durable_entry_id
        && left.raw_canonical_url == right.raw_canonical_url
        && left.safe_display_url == right.safe_display_url
        && left.restart_policy == right.restart_policy
        && left.replayable_canonical_url == right.replayable_canonical_url
        && left.originating_call_id == right.originating_call_id
        && left.provenance == right.provenance
        && left.issued_at_ms == right.issued_at_ms
        && left.expires_at_ms == right.expires_at_ms
}

fn registration_matches_capability(
    registration: &UserUrlCapabilityRegistration,
    capability: &WebUrlCapability,
) -> bool {
    registration.source_id == capability.source_id
        && registration.durable_entry_id == capability.durable_entry_id
        && registration.raw_canonical_url == capability.raw_canonical_url
        && registration.safe_display_url == capability.safe_display_url
        && registration.restart_policy == capability.restart_policy
        && registration.originating_call_id == capability.originating_call_id
        && registration.provenance == capability.provenance
        && registration.issued_at_ms == capability.issued_at_ms
        && registration.expires_at_ms == capability.expires_at_ms
}

fn capability_matches_descriptor(
    capability: &WebUrlCapability,
    descriptor: &WebUrlCapabilityDescriptor,
) -> bool {
    capability.session_scope_id == descriptor.session_scope_id
        && capability.source_id == descriptor.source_id
        && capability.durable_entry_id == descriptor.durable_entry_id
        && capability.safe_display_url == descriptor.safe_display_url
        && capability.restart_policy == descriptor.restart_policy
        && capability.originating_call_id == descriptor.originating_call_id
        && capability.provenance == descriptor.provenance
        && capability.issued_at_ms == descriptor.issued_at_ms
        && capability.expires_at_ms == descriptor.expires_at_ms
        && match descriptor.restart_policy {
            ToolRestartPolicy::Replayable => descriptor
                .replayable_canonical_url
                .as_deref()
                .is_some_and(|url| url == capability.raw_canonical_url.expose_secret()),
            ToolRestartPolicy::InterruptOnRestart => descriptor.replayable_canonical_url.is_none(),
        }
}

fn is_session_local_source_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("src_") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/url_capability_tests.rs"]
mod tests;
