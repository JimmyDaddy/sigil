use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use sigil_kernel::RootConfig;
use sigil_provider_http::{
    ProviderHttpClientOptions, ProviderHttpRedirectPolicy, build_provider_http_client_with_options,
};
use tokio::{runtime::Runtime, task::JoinHandle};

use crate::{
    ProviderStatusConfig,
    provider_connections::{
        ConfiguredProviderCredentialStore, ModelCatalogRequest, ModelCatalogResult,
        ModelCatalogState, PreparedCredential, ProcessCredentialEnvironment,
        ProviderModelCatalogService,
    },
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BalanceSnapshot {
    pub total: Option<f64>,
    pub currency: Option<String>,
    pub available: bool,
    pub status: String,
}

#[derive(Debug)]
pub enum ProviderStatusTaskResult {
    Balance {
        request_id: u64,
        snapshot: BalanceSnapshot,
    },
    Models {
        request_id: u64,
        base_url: String,
        result: std::result::Result<Vec<String>, String>,
    },
    ConnectionModels {
        request_id: u64,
        result: ModelCatalogResult,
    },
}

#[derive(Default)]
pub struct ProviderStatusTaskManager {
    active_balance_refresh: Option<ActiveProviderStatusTask>,
    active_model_refresh: Option<ActiveProviderStatusTask>,
    retired: Vec<JoinHandle<()>>,
    task_panicked: bool,
    connection_catalog_services: HashMap<PathBuf, ProviderModelCatalogService>,
}

/// A provider observation owner's shutdown could not establish successful task cleanup.
/// This reports local task ownership only; it does not change provider or model state.
#[derive(Debug, thiserror::Error)]
pub enum ProviderStatusShutdownError {
    #[error(
        "provider status shutdown deadline exceeded; pending_tasks={pending_tasks}; cleanup_complete=false"
    )]
    DeadlineExceeded { pending_tasks: usize },
    #[error("provider status task panicked during shutdown; cleanup_complete=false")]
    TaskPanicked,
}

struct ActiveProviderStatusTask {
    request_id: u64,
    handle: JoinHandle<()>,
}

impl ProviderStatusTaskManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn refresh_balance(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        provider_config: ProviderStatusConfig,
        result_tx: mpsc::Sender<ProviderStatusTaskResult>,
    ) {
        abort_task(
            &mut self.active_balance_refresh,
            &mut self.retired,
            &mut self.task_panicked,
        );
        let handle = runtime.spawn(async move {
            let snapshot = fetch_provider_balance_snapshot(&provider_config)
                .await
                .unwrap_or(BalanceSnapshot {
                    status: "balance unavailable".to_owned(),
                    ..BalanceSnapshot::default()
                });
            let _ = result_tx.send(ProviderStatusTaskResult::Balance {
                request_id,
                snapshot,
            });
        });
        self.active_balance_refresh = Some(ActiveProviderStatusTask { request_id, handle });
    }

    pub fn refresh_models(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        provider_config: ProviderStatusConfig,
        result_tx: mpsc::Sender<ProviderStatusTaskResult>,
    ) {
        abort_task(
            &mut self.active_model_refresh,
            &mut self.retired,
            &mut self.task_panicked,
        );
        let base_url = provider_config.base_url.clone();
        let handle = runtime.spawn(async move {
            let result = fetch_remote_model_ids(&provider_config)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = result_tx.send(ProviderStatusTaskResult::Models {
                request_id,
                base_url,
                result,
            });
        });
        self.active_model_refresh = Some(ActiveProviderStatusTask { request_id, handle });
    }

    pub fn refresh_connection_models(
        &mut self,
        runtime: &Runtime,
        cache_root: PathBuf,
        root_config: RootConfig,
        request: ModelCatalogRequest,
        prepared_credential: Option<PreparedCredential>,
        result_tx: mpsc::Sender<ProviderStatusTaskResult>,
    ) {
        abort_task(
            &mut self.active_model_refresh,
            &mut self.retired,
            &mut self.task_panicked,
        );
        let request_id = request.request_id;
        let service = if let Some(service) = self.connection_catalog_services.get(&cache_root) {
            Some(service.clone())
        } else {
            ProviderModelCatalogService::new(
                cache_root.clone(),
                Arc::new(ConfiguredProviderCredentialStore::from_root_config(
                    &root_config,
                )),
                Arc::new(ProcessCredentialEnvironment),
            )
            .ok()
            .inspect(|service| {
                self.connection_catalog_services
                    .insert(cache_root, service.clone());
            })
        };
        let handle = runtime.spawn(async move {
            let result = match service {
                Some(service) => {
                    service
                        .models_with_prepared_credential(
                            &root_config,
                            request,
                            prepared_credential.as_ref(),
                        )
                        .await
                }
                None => ModelCatalogResult {
                    request_id: request.request_id,
                    connection_id: request.connection_id,
                    draft_revision: request.draft_revision,
                    connection_fingerprint: request.connection_fingerprint,
                    state: ModelCatalogState::Malformed,
                    entries: Vec::new(),
                    retry_after_secs: None,
                    manual_entry_allowed: true,
                },
            };
            let _ =
                result_tx.send(ProviderStatusTaskResult::ConnectionModels { request_id, result });
        });
        self.active_model_refresh = Some(ActiveProviderStatusTask { request_id, handle });
    }

    pub fn accept_balance_result(&mut self, request_id: u64) -> bool {
        accept_result(
            &mut self.active_balance_refresh,
            request_id,
            &mut self.retired,
            &mut self.task_panicked,
        )
    }

    pub fn accept_models_result(&mut self, request_id: u64) -> bool {
        accept_result(
            &mut self.active_model_refresh,
            request_id,
            &mut self.retired,
            &mut self.task_panicked,
        )
    }

    pub fn cancel_models_refresh(&mut self, request_id: u64) {
        if self
            .active_model_refresh
            .as_ref()
            .is_some_and(|task| task.request_id == request_id)
        {
            abort_task(
                &mut self.active_model_refresh,
                &mut self.retired,
                &mut self.task_panicked,
            );
        }
    }

    pub fn abort_all(&mut self) {
        abort_task(
            &mut self.active_balance_refresh,
            &mut self.retired,
            &mut self.task_panicked,
        );
        abort_task(
            &mut self.active_model_refresh,
            &mut self.retired,
            &mut self.task_panicked,
        );
    }

    /// Cancels observation work and confirms task completion within the caller's shared budget.
    /// Pending handles remain owned on timeout; aborting alone never establishes quiescence.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderStatusShutdownError::DeadlineExceeded`] if any owned task remains
    /// unfinished at `deadline`. Returns [`ProviderStatusShutdownError::TaskPanicked`] if a
    /// task panicked, including a panic observed during earlier nonblocking result reaping.
    /// Panic evidence stays latched across repeated calls; unfinished handles remain owned.
    pub async fn shutdown_until(
        &mut self,
        deadline: Instant,
    ) -> std::result::Result<(), ProviderStatusShutdownError> {
        self.abort_all();
        reap_finished_tasks(&mut self.retired, &mut self.task_panicked);
        while let Some(handle) = self.retired.last_mut() {
            match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), handle).await {
                Ok(result) => {
                    self.task_panicked |= result.is_err_and(|error| error.is_panic());
                    self.retired.pop();
                }
                Err(_) => {
                    return Err(ProviderStatusShutdownError::DeadlineExceeded {
                        pending_tasks: self.retired.len(),
                    });
                }
            }
        }
        if self.task_panicked {
            Err(ProviderStatusShutdownError::TaskPanicked)
        } else {
            Ok(())
        }
    }
}

impl Drop for ProviderStatusTaskManager {
    fn drop(&mut self) {
        self.abort_all();
    }
}

pub async fn fetch_remote_model_ids(config: &ProviderStatusConfig) -> Result<Vec<String>> {
    let (api_key, url, timeout_secs) = provider_status_request_parts(config, "models")?;
    let client = build_provider_status_client(timeout_secs, "model-list")?;
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch provider models: {error}"))?;
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| anyhow!("failed to decode provider models: {error}"))?;
    let models = parse_remote_model_ids(&payload)?;
    Ok(models)
}

fn accept_result(
    active: &mut Option<ActiveProviderStatusTask>,
    request_id: u64,
    retired: &mut Vec<JoinHandle<()>>,
    task_panicked: &mut bool,
) -> bool {
    if active
        .as_ref()
        .is_some_and(|task| task.request_id == request_id)
    {
        if let Some(task) = active.take() {
            retain_task(retired, task.handle, task_panicked);
        }
        true
    } else {
        false
    }
}

fn abort_task(
    active: &mut Option<ActiveProviderStatusTask>,
    retired: &mut Vec<JoinHandle<()>>,
    task_panicked: &mut bool,
) {
    if let Some(task) = active.take() {
        task.handle.abort();
        retain_task(retired, task.handle, task_panicked);
    }
}

fn retain_task(
    retired: &mut Vec<JoinHandle<()>>,
    handle: JoinHandle<()>,
    task_panicked: &mut bool,
) {
    retired.push(handle);
    reap_finished_tasks(retired, task_panicked);
}

fn reap_finished_tasks(retired: &mut Vec<JoinHandle<()>>, task_panicked: &mut bool) {
    retired.retain_mut(|handle| {
        if !handle.is_finished() {
            return true;
        }
        match futures::FutureExt::now_or_never(handle) {
            Some(result) => {
                *task_panicked |= result.is_err_and(|error| error.is_panic());
                false
            }
            None => true,
        }
    });
}

pub async fn fetch_provider_balance_snapshot(
    config: &ProviderStatusConfig,
) -> Result<BalanceSnapshot> {
    let (api_key, url, timeout_secs) = provider_status_request_parts(config, "user/balance")?;
    let client = build_provider_status_client(timeout_secs, "balance")?;
    let payload = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch balance: {error}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|error| anyhow!("failed to decode balance payload: {error}"))?;
    parse_balance_snapshot(&payload)
}

#[cfg_attr(coverage, allow(dead_code))]
fn parse_remote_model_ids(payload: &serde_json::Value) -> Result<Vec<String>> {
    let items = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("provider model response is missing a data array"))?;
    let mut model_ids = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let model_id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|model_id| !model_id.is_empty())
            .ok_or_else(|| {
                anyhow!("provider model response item {index} is missing a non-empty string id")
            })?;
        if !model_ids.iter().any(|existing| existing == model_id) {
            model_ids.push(model_id.to_owned());
        }
    }
    Ok(model_ids)
}

fn provider_request_timeout_secs(config: &ProviderStatusConfig) -> u64 {
    config.request_timeout_secs.clamp(1, 5)
}

fn provider_status_request_parts(
    config: &ProviderStatusConfig,
    path_suffix: &str,
) -> Result<(String, String, u64)> {
    let api_key = require_provider_auth(resolve_provider_api_key(config))?;
    let url = provider_status_url(config, path_suffix);
    let timeout_secs = provider_request_timeout_secs(config);
    Ok((api_key, url, timeout_secs))
}

fn require_provider_auth(api_key: Option<String>) -> Result<String> {
    api_key.ok_or_else(|| anyhow!("missing auth"))
}

fn provider_status_url(config: &ProviderStatusConfig, path_suffix: &str) -> String {
    format!(
        "{}/{}",
        config.base_url.trim_end_matches('/'),
        path_suffix.trim_start_matches('/')
    )
}

fn build_provider_status_client(timeout_secs: u64, label: &str) -> Result<Client> {
    build_provider_http_client_with_options(ProviderHttpClientOptions {
        timeout: Some(Duration::from_secs(timeout_secs)),
        redirect: ProviderHttpRedirectPolicy::Default,
        referer: true,
    })
    .map_err(|error| anyhow!("failed to build {label} client: {error}"))
}

fn parse_balance_snapshot(payload: &serde_json::Value) -> Result<BalanceSnapshot> {
    let available = payload
        .get("is_available")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let Some(items) = payload
        .get("balance_infos")
        .and_then(serde_json::Value::as_array)
    else {
        bail!("provider returned no balance infos");
    };
    let primary = items
        .iter()
        .filter_map(|item| {
            let currency = item.get("currency")?.as_str()?.to_owned();
            let total = item
                .get("total_balance")?
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())?;
            Some((currency, total))
        })
        .next();

    let Some((currency, total)) = primary else {
        bail!("provider returned no parseable balances");
    };
    Ok(BalanceSnapshot {
        total: Some(total),
        currency: Some(currency.clone()),
        available,
        status: if available {
            format!("{currency} {total:.2}")
        } else {
            "unavailable".to_owned()
        },
    })
}

pub(crate) fn resolve_provider_api_key(config: &ProviderStatusConfig) -> Option<String> {
    config.api_key.clone()
}

#[cfg(test)]
#[path = "tests/provider_status_tests.rs"]
mod tests;
