use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Weak},
};

use sigil_mcp::{
    McpOAuthCredentialError, McpOAuthCredentialLocatorStore, McpOAuthCredentialLookup,
    McpOAuthCredentialRecord, McpOAuthCredentialScope, McpOAuthCredentialSnapshot,
    McpOAuthCredentialStatus, McpOAuthCredentialStore, McpOAuthHttpExecutor,
    McpOAuthRevocationOutcome, McpStreamableHttpBearerProvider, McpStreamableHttpDestinationError,
    McpStreamableHttpError, SystemMcpOAuthCredentialStore, refresh_oauth_credential,
    revoke_oauth_credential,
};
use tokio::sync::Mutex;

/// Per-request bridge from a public OAuth lookup to exact keyring credentials and durable refresh.
pub struct RuntimeMcpOAuthBearerProvider {
    manager: Arc<McpOAuthCredentialManager>,
    lookup: McpOAuthCredentialLookup,
    executor: Arc<dyn McpOAuthHttpExecutor>,
}

impl std::fmt::Debug for RuntimeMcpOAuthBearerProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeMcpOAuthBearerProvider")
            .field("lookup", &self.lookup)
            .finish_non_exhaustive()
    }
}

impl RuntimeMcpOAuthBearerProvider {
    #[must_use]
    pub fn new(
        manager: Arc<McpOAuthCredentialManager>,
        lookup: McpOAuthCredentialLookup,
        executor: Arc<dyn McpOAuthHttpExecutor>,
    ) -> Self {
        Self {
            manager,
            lookup,
            executor,
        }
    }

    #[must_use]
    pub fn lookup(&self) -> &McpOAuthCredentialLookup {
        &self.lookup
    }
}

#[async_trait::async_trait]
impl McpStreamableHttpBearerProvider for RuntimeMcpOAuthBearerProvider {
    async fn bearer_snapshot(
        &self,
        static_header_fingerprint: &str,
    ) -> Result<McpOAuthCredentialSnapshot, McpStreamableHttpError> {
        let record = self
            .manager
            .load_for_lookup(&self.lookup)
            .await
            .map_err(map_credential_error)?
            .ok_or(McpStreamableHttpError::AuthenticationRequired)?;
        self.manager
            .bearer_snapshot(
                record.scope(),
                static_header_fingerprint,
                current_epoch_secs(),
                self.executor.as_ref(),
            )
            .await
            .map_err(map_credential_error)
    }

    async fn mark_unauthorized(&self) -> Result<(), McpStreamableHttpError> {
        let Some(record) = self
            .manager
            .load_for_lookup(&self.lookup)
            .await
            .map_err(map_credential_error)?
        else {
            return Ok(());
        };
        self.manager
            .mark_unauthorized(record.scope(), current_epoch_secs())
            .await
            .map_err(map_credential_error)
    }
}

fn current_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn map_credential_error(error: McpOAuthCredentialError) -> McpStreamableHttpError {
    match error {
        McpOAuthCredentialError::AuthenticationRequired
        | McpOAuthCredentialError::InvalidRefresh => McpStreamableHttpError::AuthenticationRequired,
        McpOAuthCredentialError::DestinationRejected => {
            McpStreamableHttpError::DestinationAuthorization(
                McpStreamableHttpDestinationError::DestinationRejected,
            )
        }
        McpOAuthCredentialError::BudgetExhausted => McpStreamableHttpError::BudgetExhausted,
        McpOAuthCredentialError::Transport => McpStreamableHttpError::Transport,
        McpOAuthCredentialError::InvalidScope
        | McpOAuthCredentialError::InvalidRecord
        | McpOAuthCredentialError::StoreUnavailable
        | McpOAuthCredentialError::StoreRejected
        | McpOAuthCredentialError::RefreshRejected
        | McpOAuthCredentialError::RevocationRejected => {
            McpStreamableHttpError::AuthenticationFailed
        }
    }
}

/// Runtime owner for keyring persistence and per-scope single-flight OAuth refresh.
pub struct McpOAuthCredentialManager {
    store: Arc<dyn McpOAuthCredentialStore>,
    locator_store: Option<Arc<dyn McpOAuthCredentialLocatorStore>>,
    refresh_locks: Mutex<BTreeMap<String, Weak<Mutex<()>>>>,
}

impl fmt::Debug for McpOAuthCredentialManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCredentialManager")
            .finish_non_exhaustive()
    }
}

impl Default for McpOAuthCredentialManager {
    fn default() -> Self {
        Self::system()
    }
}

impl McpOAuthCredentialManager {
    /// Uses the native system credential store. There is deliberately no file fallback.
    #[must_use]
    pub fn system() -> Self {
        let store = Arc::new(SystemMcpOAuthCredentialStore);
        Self {
            store: store.clone(),
            locator_store: Some(store),
            refresh_locks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Builds a manager around a credential store implementation, primarily for conformance tests.
    #[must_use]
    pub fn new(store: Arc<dyn McpOAuthCredentialStore>) -> Self {
        Self {
            store,
            locator_store: None,
            refresh_locks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Builds a manager with a keyring lookup index, primarily for end-to-end flow tests.
    #[must_use]
    pub fn new_with_locator(
        store: Arc<dyn McpOAuthCredentialStore>,
        locator_store: Arc<dyn McpOAuthCredentialLocatorStore>,
    ) -> Self {
        Self {
            store,
            locator_store: Some(locator_store),
            refresh_locks: Mutex::new(BTreeMap::new()),
        }
    }

    /// Persists one complete versioned record before it can be used by MCP requests.
    pub async fn persist(
        &self,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        self.store.store(record).await
    }

    /// Persists the exact credential first, then commits the public-intent locator.
    pub async fn persist_for_lookup(
        &self,
        lookup: &McpOAuthCredentialLookup,
        record: &McpOAuthCredentialRecord,
    ) -> Result<(), McpOAuthCredentialError> {
        let locator = self
            .locator_store
            .as_ref()
            .ok_or(McpOAuthCredentialError::StoreUnavailable)?;
        let previous = self.store.load(record.scope()).await?;
        self.store.store(record).await?;
        if let Err(error) = locator.store_locator(lookup, record.scope()).await {
            match previous {
                Some(previous) => {
                    let _ = self.store.store(&previous).await;
                }
                None => {
                    let _ = self.store.delete(record.scope()).await;
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Resolves the exact secret-bearing record through the keyring-only intent locator.
    pub async fn load_for_lookup(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<Option<McpOAuthCredentialRecord>, McpOAuthCredentialError> {
        self.locator_store
            .as_ref()
            .ok_or(McpOAuthCredentialError::StoreUnavailable)?
            .load_located(lookup)
            .await
    }

    /// Returns the located exact scope and its secret-free status.
    pub async fn status_for_lookup(
        &self,
        lookup: &McpOAuthCredentialLookup,
        now_epoch_secs: u64,
    ) -> Result<Option<(McpOAuthCredentialScope, McpOAuthCredentialStatus)>, McpOAuthCredentialError>
    {
        Ok(self
            .load_for_lookup(lookup)
            .await?
            .map(|record| (record.scope().clone(), record.status(now_epoch_secs))))
    }

    /// Returns a secret-free status projection; unavailable stores never become `Missing`.
    pub async fn status(
        &self,
        scope: &McpOAuthCredentialScope,
        now_epoch_secs: u64,
    ) -> McpOAuthCredentialStatus {
        match self.store.load(scope).await {
            Ok(Some(record)) => record.status(now_epoch_secs),
            Ok(None) => McpOAuthCredentialStatus::Missing,
            Err(_) => McpOAuthCredentialStatus::Unavailable,
        }
    }

    /// Returns a current bearer snapshot, refreshing once per scope when inside the expiry skew.
    pub async fn bearer_snapshot(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_and_snapshot(scope, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    /// Explicitly refreshes a credential without retrying any MCP request.
    pub async fn refresh_now(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let observed = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?
            .generation_id()
            .to_owned();
        let lock = self.refresh_lock(scope).await;
        let _guard = lock.lock().await;
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.generation_id() != observed
            && record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present
        {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_owned(record, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    /// Marks the current access snapshot unusable after a 401. It does not refresh or retry.
    pub async fn mark_unauthorized(
        &self,
        scope: &McpOAuthCredentialScope,
        now_epoch_secs: u64,
    ) -> Result<(), McpOAuthCredentialError> {
        let Some(record) = self.store.load(scope).await? else {
            return Ok(());
        };
        self.store
            .store(&record.without_access_token(now_epoch_secs))
            .await
    }

    /// Attempts remote revocation without deleting the local keyring record.
    pub async fn revoke(
        &self,
        scope: &McpOAuthCredentialScope,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthRevocationOutcome, McpOAuthCredentialError> {
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        revoke_oauth_credential(executor, &record).await
    }

    /// Deletes only the local system-keyring record and makes no remote-revocation claim.
    pub async fn clear_local(
        &self,
        scope: &McpOAuthCredentialScope,
    ) -> Result<bool, McpOAuthCredentialError> {
        self.store.delete(scope).await
    }

    /// Clears both the exact credential and its public-intent locator.
    pub async fn clear_local_for_lookup(
        &self,
        lookup: &McpOAuthCredentialLookup,
    ) -> Result<bool, McpOAuthCredentialError> {
        let locator = self
            .locator_store
            .as_ref()
            .ok_or(McpOAuthCredentialError::StoreUnavailable)?;
        let record = locator.load_located(lookup).await?;
        let mut deleted = false;
        if let Some(record) = record {
            deleted = self.store.delete(record.scope()).await?;
        }
        let locator_deleted = locator.delete_locator(lookup).await?;
        Ok(deleted || locator_deleted)
    }

    async fn refresh_and_snapshot(
        &self,
        scope: &McpOAuthCredentialScope,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        let lock = self.refresh_lock(scope).await;
        let _guard = lock.lock().await;
        let record = self
            .store
            .load(scope)
            .await?
            .ok_or(McpOAuthCredentialError::AuthenticationRequired)?;
        if record.status(now_epoch_secs) == McpOAuthCredentialStatus::Present {
            return record.snapshot(static_header_fingerprint, now_epoch_secs);
        }
        self.refresh_owned(record, static_header_fingerprint, now_epoch_secs, executor)
            .await
    }

    async fn refresh_owned(
        &self,
        record: McpOAuthCredentialRecord,
        static_header_fingerprint: &str,
        now_epoch_secs: u64,
        executor: &dyn McpOAuthHttpExecutor,
    ) -> Result<McpOAuthCredentialSnapshot, McpOAuthCredentialError> {
        if !record.can_refresh(now_epoch_secs) {
            return Err(McpOAuthCredentialError::AuthenticationRequired);
        }
        match refresh_oauth_credential(executor, &record).await {
            Ok(token) => {
                let next = record.rotated(&token, now_epoch_secs)?;
                self.store.store(&next).await?;
                next.snapshot(static_header_fingerprint, now_epoch_secs)
            }
            Err(McpOAuthCredentialError::InvalidRefresh) => {
                self.store
                    .store(&record.without_usable_tokens(now_epoch_secs))
                    .await?;
                Err(McpOAuthCredentialError::AuthenticationRequired)
            }
            Err(error) => Err(error),
        }
    }

    async fn refresh_lock(&self, scope: &McpOAuthCredentialScope) -> Arc<Mutex<()>> {
        let mut locks = self.refresh_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(scope.binding_id()).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(scope.binding_id().to_owned(), Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
#[path = "tests/mcp_oauth_tests.rs"]
mod tests;
