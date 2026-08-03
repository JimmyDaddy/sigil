use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{ConnectionId, ModelRef, RootConfig};
use tokio::sync::Mutex;

use super::catalog_cache::{
    CATALOG_FRESH_TTL_SECS, CATALOG_STALE_WINDOW_SECS, CachedCatalog, cache_age_secs,
    load_catalog_cache, save_catalog_cache, sweep_catalog_cache,
};
use super::{
    CredentialEnvironment, CredentialRefConfig, LoadedCredentialRef, PreparedCredential,
    ProviderConnectionConfig, ProviderCredentialStore, ProviderFamily, ProviderProtocol,
    ResolvedCredential, ResolvedCredentialSource, load_provider_connections,
    resolve_connection_credential,
};

const CATALOG_BODY_MAX_BYTES: usize = 1024 * 1024;
const CATALOG_ENTRY_MAX: usize = 2_000;
const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);
const CATALOG_TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
const CATALOG_PAGE_MAX: usize = 100;
const CATALOG_FLIGHT_SOFT_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogProvenance {
    Remote,
    Cache,
    Bundled,
    Configured,
    Manual,
}

impl ModelCatalogProvenance {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Cache => "cache",
            Self::Bundled => "bundled",
            Self::Configured => "configured",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    Unverified,
    ConfiguredUnavailable,
}

impl ModelAvailability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unverified => "unverified",
            Self::ConfiguredUnavailable => "configured_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRecommendation {
    Recommended,
    Standard,
}

impl ModelRecommendation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Standard => "standard",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogEntry {
    pub model_ref: ModelRef,
    pub display_name: String,
    pub availability: ModelAvailability,
    pub recommendation: ModelRecommendation,
    pub provenance: ModelCatalogProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogState {
    Remote,
    CacheFresh,
    CacheStale,
    Bundled,
    Empty,
    AuthRejected,
    Offline,
    Unsupported,
    Malformed,
    TlsRejected,
    ProtocolMismatch,
    RateLimited,
    CredentialUnavailable,
}

impl ModelCatalogState {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::CacheFresh => "cache_fresh",
            Self::CacheStale => "cache_stale",
            Self::Bundled => "bundled",
            Self::Empty => "remote_empty",
            Self::AuthRejected => "auth_rejected",
            Self::Offline => "offline",
            Self::Unsupported => "catalog_unsupported",
            Self::Malformed => "catalog_malformed",
            Self::TlsRejected => "tls_rejected",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::RateLimited => "rate_limited",
            Self::CredentialUnavailable => "credential_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelCatalogRequest {
    pub request_id: u64,
    pub connection_id: ConnectionId,
    pub draft_revision: u64,
    pub connection_fingerprint: String,
    pub explicit_refresh: bool,
}

#[derive(Debug, Clone)]
pub struct ModelCatalogResult {
    pub request_id: u64,
    pub connection_id: ConnectionId,
    pub draft_revision: u64,
    pub connection_fingerprint: String,
    pub state: ModelCatalogState,
    pub entries: Vec<ModelCatalogEntry>,
    pub retry_after_secs: Option<u64>,
    pub manual_entry_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionProbeState {
    Ready,
    CredentialMissing,
    CredentialRejected,
    EndpointUnreachable,
    TlsRejected,
    ProtocolMismatch,
    CatalogUnsupported,
    EmptyCatalog,
    MalformedResponse,
}

#[derive(Debug, Clone)]
pub struct ConnectionProbeResult {
    pub request_id: u64,
    pub connection_id: ConnectionId,
    pub draft_revision: u64,
    pub connection_fingerprint: String,
    pub state: ConnectionProbeState,
}

#[derive(Clone)]
pub struct ProviderModelCatalogService {
    cache_root: PathBuf,
    client: Client,
    credential_store: Arc<dyn ProviderCredentialStore>,
    environment: Arc<dyn CredentialEnvironment>,
    memory_cache: Arc<Mutex<HashMap<String, CachedCatalog>>>,
    flights: Arc<Mutex<HashMap<String, Arc<CatalogFlight>>>>,
}

impl ProviderModelCatalogService {
    pub fn new(
        cache_root: PathBuf,
        credential_store: Arc<dyn ProviderCredentialStore>,
        environment: Arc<dyn CredentialEnvironment>,
    ) -> anyhow::Result<Self> {
        let _ = sweep_catalog_cache(&cache_root);
        let client = Client::builder()
            .timeout(CATALOG_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .build()?;
        Ok(Self {
            cache_root,
            client,
            credential_store,
            environment,
            memory_cache: Arc::new(Mutex::new(HashMap::new())),
            flights: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn models(
        &self,
        root_config: &RootConfig,
        request: ModelCatalogRequest,
    ) -> ModelCatalogResult {
        self.models_with_prepared_credential(root_config, request, None)
            .await
    }

    pub async fn models_with_prepared_credential(
        &self,
        root_config: &RootConfig,
        request: ModelCatalogRequest,
        prepared_credential: Option<&PreparedCredential>,
    ) -> ModelCatalogResult {
        let loaded = load_provider_connections(root_config);
        let Some(connection) = loaded.connections.get(&request.connection_id) else {
            return result_with(request, ModelCatalogState::Unsupported, Vec::new(), None);
        };
        let admitted_fingerprint = connection_semantic_fingerprint(&connection.config);
        if admitted_fingerprint != request.connection_fingerprint {
            return result_with(request, ModelCatalogState::Malformed, Vec::new(), None);
        }
        let configured_model = loaded
            .default_model
            .as_ref()
            .filter(|model| model.connection_id == request.connection_id)
            .cloned();
        let resolved = match prepared_credential {
            Some(prepared) if prepared.provider_family == connection.config.provider => {
                ResolvedCredential {
                    secret: Some(prepared.secret().clone()),
                    source: ResolvedCredentialSource::ProcessStaged,
                    generation_id: None,
                }
            }
            Some(_) => {
                return result_with(
                    request,
                    ModelCatalogState::CredentialUnavailable,
                    configured_plus_bundled(&connection.config, configured_model.as_ref()),
                    None,
                );
            }
            None => match resolve_connection_credential(
                &connection.config,
                &connection.credential,
                self.credential_store.as_ref(),
                self.environment.as_ref(),
            )
            .await
            {
                Ok(credential) => credential,
                Err(_) => {
                    return result_with(
                        request,
                        ModelCatalogState::CredentialUnavailable,
                        configured_plus_bundled(&connection.config, configured_model.as_ref()),
                        None,
                    );
                }
            },
        };
        let catalog_fingerprint =
            catalog_fingerprint(&connection.config, &connection.credential, &resolved);
        let flight_key = stable_digest(&[
            catalog_fingerprint.as_bytes(),
            configured_model
                .as_ref()
                .map(|model| model.model_id.as_bytes())
                .unwrap_or_default(),
            if request.explicit_refresh {
                b"explicit-refresh"
            } else {
                b"cached-or-remote"
            },
        ]);
        let flight = {
            let mut flights = self.flights.lock().await;
            if !flights.contains_key(&flight_key)
                && flights.len() >= CATALOG_FLIGHT_SOFT_LIMIT
                && let Some(inactive) = flights
                    .iter()
                    .find(|(_, flight)| Arc::strong_count(flight) == 1)
                    .map(|(fingerprint, _)| fingerprint.clone())
            {
                flights.remove(&inactive);
            }
            flights
                .entry(flight_key)
                .or_insert_with(|| Arc::new(CatalogFlight::default()))
                .clone()
        };
        let observed_generation = flight.generation.load(Ordering::Acquire);
        let _flight_guard = flight.lock.lock().await;
        if flight.generation.load(Ordering::Acquire) > observed_generation
            && let Some(outcome) = flight
                .last_outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        {
            return outcome.with_request(request);
        }

        if !request.explicit_refresh
            && let Some(cache) = self
                .load_cache(&connection.config, &resolved, &catalog_fingerprint)
                .await
            && cache_age_secs(&cache) <= CATALOG_FRESH_TTL_SECS
        {
            return complete_catalog_flight(
                &flight,
                result_with(
                    request,
                    ModelCatalogState::CacheFresh,
                    configured_warning(cache_entries(cache.entries), configured_model.as_ref()),
                    None,
                ),
            );
        }

        let result = match self.fetch_remote(&connection.config, &resolved).await {
            Ok(mut remote_entries) => {
                enrich_remote_entries(&connection.config, &mut remote_entries);
                self.save_cache(
                    &connection.config,
                    &resolved,
                    &catalog_fingerprint,
                    &remote_entries,
                )
                .await;
                let entries = configured_warning(remote_entries, configured_model.as_ref());
                if entries
                    .iter()
                    .all(|entry| entry.availability == ModelAvailability::ConfiguredUnavailable)
                {
                    result_with(request, ModelCatalogState::Empty, entries, None)
                } else {
                    result_with(request, ModelCatalogState::Remote, entries, None)
                }
            }
            Err(failure) => {
                let cached = self
                    .load_cache(&connection.config, &resolved, &catalog_fingerprint)
                    .await
                    .filter(|cache| cache_age_secs(cache) <= CATALOG_STALE_WINDOW_SECS);
                let (state, retry_after) = failure.state();
                if state != ModelCatalogState::Unsupported
                    && let Some(cache) = cached
                {
                    result_with(
                        request,
                        state,
                        configured_reference(
                            stale_reference_entries(cache_entries(cache.entries)),
                            configured_model.as_ref(),
                            ModelAvailability::Unverified,
                        ),
                        retry_after,
                    )
                } else {
                    result_with(
                        request,
                        state,
                        configured_plus_bundled(&connection.config, configured_model.as_ref()),
                        retry_after,
                    )
                }
            }
        };
        complete_catalog_flight(&flight, result)
    }

    pub async fn probe(
        &self,
        root_config: &RootConfig,
        mut request: ModelCatalogRequest,
        prepared_credential: Option<&PreparedCredential>,
    ) -> ConnectionProbeResult {
        request.explicit_refresh = true;
        let result = self
            .models_with_prepared_credential(root_config, request, prepared_credential)
            .await;
        let state = match result.state {
            ModelCatalogState::Remote | ModelCatalogState::CacheFresh => {
                ConnectionProbeState::Ready
            }
            ModelCatalogState::Empty => ConnectionProbeState::EmptyCatalog,
            ModelCatalogState::AuthRejected => ConnectionProbeState::CredentialRejected,
            ModelCatalogState::CredentialUnavailable => ConnectionProbeState::CredentialMissing,
            ModelCatalogState::Unsupported | ModelCatalogState::Bundled => {
                ConnectionProbeState::CatalogUnsupported
            }
            ModelCatalogState::Malformed => ConnectionProbeState::MalformedResponse,
            ModelCatalogState::TlsRejected => ConnectionProbeState::TlsRejected,
            ModelCatalogState::ProtocolMismatch => ConnectionProbeState::ProtocolMismatch,
            ModelCatalogState::Offline
            | ModelCatalogState::CacheStale
            | ModelCatalogState::RateLimited => ConnectionProbeState::EndpointUnreachable,
        };
        ConnectionProbeResult {
            request_id: result.request_id,
            connection_id: result.connection_id,
            draft_revision: result.draft_revision,
            connection_fingerprint: result.connection_fingerprint,
            state,
        }
    }

    async fn load_cache(
        &self,
        connection: &ProviderConnectionConfig,
        credential: &ResolvedCredential,
        fingerprint: &str,
    ) -> Option<CachedCatalog> {
        if credential_is_process_local(credential.source) {
            return self.memory_cache.lock().await.get(fingerprint).cloned();
        }
        let mut cached = load_catalog_cache(&self.cache_root, connection.id.as_str(), fingerprint)?;
        enrich_remote_entries(connection, &mut cached.entries);
        Some(cached)
    }

    async fn save_cache(
        &self,
        connection: &ProviderConnectionConfig,
        credential: &ResolvedCredential,
        fingerprint: &str,
        entries: &[ModelCatalogEntry],
    ) {
        let cache = CachedCatalog {
            stored_at_unix_secs: now_unix_secs(),
            entries: entries.to_vec(),
        };
        if credential_is_process_local(credential.source) {
            self.memory_cache
                .lock()
                .await
                .insert(fingerprint.to_owned(), cache);
            return;
        }
        let _ = save_catalog_cache(
            &self.cache_root,
            connection.id.as_str(),
            fingerprint,
            entries,
        );
    }

    async fn fetch_remote(
        &self,
        connection: &ProviderConnectionConfig,
        credential: &ResolvedCredential,
    ) -> Result<Vec<ModelCatalogEntry>, CatalogFailure> {
        tokio::time::timeout(
            CATALOG_TOTAL_TIMEOUT,
            self.fetch_remote_pages(connection, credential),
        )
        .await
        .map_err(|_| CatalogFailure::Offline)?
    }

    async fn fetch_remote_pages(
        &self,
        connection: &ProviderConnectionConfig,
        credential: &ResolvedCredential,
    ) -> Result<Vec<ModelCatalogEntry>, CatalogFailure> {
        let mut entries = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut page_count = 0usize;
        loop {
            page_count += 1;
            admit_catalog_page(page_count)?;
            let request =
                provider_catalog_request(&self.client, connection, credential, cursor.as_deref())?;
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) if error.is_connect() || error.is_timeout() => provider_catalog_request(
                    &self.client,
                    connection,
                    credential,
                    cursor.as_deref(),
                )?
                .send()
                .await
                .map_err(|error| classify_transport_failure(&error))?,
                Err(error) => return Err(classify_transport_failure(&error)),
            };
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(CatalogFailure::AuthRejected);
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after_secs = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|value| value.min(3600));
                return Err(CatalogFailure::RateLimited(retry_after_secs));
            }
            if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
                return Err(CatalogFailure::Unsupported);
            }
            if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
                return Err(CatalogFailure::ProtocolMismatch);
            }
            if status.is_redirection() {
                return Err(CatalogFailure::Malformed);
            }
            if !status.is_success() {
                return Err(CatalogFailure::Offline);
            }
            let bytes = read_bounded_body(response).await?;
            let page = map_provider_catalog_page(connection, &bytes)?;
            entries.extend(page.entries);
            if entries.len() > CATALOG_ENTRY_MAX {
                return Err(CatalogFailure::Malformed);
            }
            let Some(next) = admit_catalog_next_cursor(&mut seen_cursors, page.next)? else {
                break;
            };
            cursor = Some(next);
        }
        deduplicate_entries(&mut entries);
        Ok(entries)
    }
}

#[derive(Default)]
struct CatalogFlight {
    lock: Mutex<()>,
    generation: AtomicU64,
    last_outcome: StdMutex<Option<CatalogFlightOutcome>>,
}

#[derive(Clone)]
struct CatalogFlightOutcome {
    state: ModelCatalogState,
    entries: Vec<ModelCatalogEntry>,
    retry_after_secs: Option<u64>,
}

impl CatalogFlightOutcome {
    fn with_request(self, request: ModelCatalogRequest) -> ModelCatalogResult {
        result_with(request, self.state, self.entries, self.retry_after_secs)
    }
}

fn complete_catalog_flight(
    flight: &CatalogFlight,
    result: ModelCatalogResult,
) -> ModelCatalogResult {
    *flight
        .last_outcome
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CatalogFlightOutcome {
        state: result.state,
        entries: result.entries.clone(),
        retry_after_secs: result.retry_after_secs,
    });
    flight.generation.fetch_add(1, Ordering::Release);
    result
}

#[derive(Debug)]
struct ProviderCatalogPage {
    entries: Vec<ModelCatalogEntry>,
    next: Option<String>,
}

pub(super) fn admit_catalog_page(page_count: usize) -> Result<(), CatalogFailure> {
    if page_count > CATALOG_PAGE_MAX {
        return Err(CatalogFailure::Malformed);
    }
    Ok(())
}

pub(super) fn admit_catalog_next_cursor(
    seen_cursors: &mut HashSet<String>,
    next: Option<String>,
) -> Result<Option<String>, CatalogFailure> {
    let Some(next) = next else {
        return Ok(None);
    };
    if next.is_empty() || !seen_cursors.insert(next.clone()) {
        return Err(CatalogFailure::Malformed);
    }
    Ok(Some(next))
}

fn provider_catalog_request(
    client: &Client,
    connection: &ProviderConnectionConfig,
    credential: &ResolvedCredential,
    cursor: Option<&str>,
) -> Result<reqwest::RequestBuilder, CatalogFailure> {
    let secret = credential
        .secret
        .as_ref()
        .map(sigil_kernel::SecretString::expose_secret);
    let base = connection.base_url.trim_end_matches('/');
    let request = match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek)
        | (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => {
            let request = client.get(format!("{base}/models"));
            if let Some(secret) = secret {
                request.bearer_auth(secret)
            } else {
                request
            }
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            let models_url = if base.ends_with("/v1") {
                format!("{base}/models")
            } else {
                format!("{base}/v1/models")
            };
            let mut request = client
                .get(models_url)
                .header("anthropic-version", anthropic_version(connection))
                .query(&[("limit", "1000")]);
            if let Some(cursor) = cursor {
                request = request.query(&[("after_id", cursor)]);
            }
            if let Some(secret) = secret {
                request.header("x-api-key", secret)
            } else {
                request
            }
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            let mut request = client
                .get(format!("{base}/models"))
                .query(&[("pageSize", "1000")]);
            if let Some(cursor) = cursor {
                request = request.query(&[("pageToken", cursor)]);
            }
            if let Some(secret) = secret {
                request.header("x-goog-api-key", secret)
            } else {
                request
            }
        }
        _ => return Err(CatalogFailure::Unsupported),
    };
    Ok(request.header(reqwest::header::ACCEPT, "application/json"))
}

fn map_provider_catalog_page(
    connection: &ProviderConnectionConfig,
    bytes: &[u8],
) -> Result<ProviderCatalogPage, CatalogFailure> {
    let id = connection.id.clone();
    let mapped = match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => ProviderCatalogPage {
            entries: sigil_provider_deepseek::parse_deepseek_model_list(bytes)
                .map_err(|_| CatalogFailure::Malformed)?
                .into_iter()
                .map(|model| remote_entry(id.clone(), model.id, None, true))
                .collect::<Result<_, _>>()?,
            next: None,
        },
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => ProviderCatalogPage {
            entries: sigil_provider_openai_responses::parse_openai_responses_model_list(bytes)
                .map_err(|_| CatalogFailure::Malformed)?
                .into_iter()
                .map(|model| {
                    let verified = model.admission
                        == sigil_provider_openai_responses::OpenAiModelAdmission::KnownGeneration;
                    remote_entry(id.clone(), model.id, None, verified)
                })
                .collect::<Result<_, _>>()?,
            next: None,
        },
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => ProviderCatalogPage {
            entries: sigil_provider_openai_compat::parse_openai_compatible_model_list(bytes)
                .map_err(|_| CatalogFailure::Malformed)?
                .into_iter()
                .map(|model| remote_entry(id.clone(), model.id, None, false))
                .collect::<Result<_, _>>()?,
            next: None,
        },
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            let page = sigil_provider_anthropic::parse_anthropic_model_list(bytes)
                .map_err(|_| CatalogFailure::Malformed)?;
            ProviderCatalogPage {
                entries: page
                    .models
                    .into_iter()
                    .map(|model| remote_entry(id.clone(), model.id, Some(model.display_name), true))
                    .collect::<Result<_, _>>()?,
                next: page.next_after_id,
            }
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            let page = sigil_provider_gemini::parse_gemini_model_list(bytes)
                .map_err(|_| CatalogFailure::Malformed)?;
            ProviderCatalogPage {
                entries: page
                    .models
                    .into_iter()
                    .map(|model| remote_entry(id.clone(), model.id, Some(model.display_name), true))
                    .collect::<Result<_, _>>()?,
                next: page.next_page_token,
            }
        }
        _ => return Err(CatalogFailure::Unsupported),
    };
    Ok(mapped)
}

fn remote_entry(
    connection_id: ConnectionId,
    model_id: String,
    display_name: Option<String>,
    verified: bool,
) -> Result<ModelCatalogEntry, CatalogFailure> {
    let model_ref =
        ModelRef::new(connection_id, model_id).map_err(|_| CatalogFailure::Malformed)?;
    let display_name = display_name.unwrap_or_else(|| model_ref.model_id.clone());
    validate_display_name(&display_name)?;
    Ok(ModelCatalogEntry {
        model_ref,
        display_name,
        availability: if verified {
            ModelAvailability::Available
        } else {
            ModelAvailability::Unverified
        },
        recommendation: ModelRecommendation::Standard,
        provenance: ModelCatalogProvenance::Remote,
    })
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>, CatalogFailure> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CatalogFailure::Offline)?;
        if body.len().saturating_add(chunk.len()) > CATALOG_BODY_MAX_BYTES {
            return Err(CatalogFailure::Malformed);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn configured_plus_bundled(
    connection: &ProviderConnectionConfig,
    configured: Option<&ModelRef>,
) -> Vec<ModelCatalogEntry> {
    configured_reference(
        bundled_model_entries(connection),
        configured,
        ModelAvailability::Unverified,
    )
}

#[must_use]
pub fn bundled_model_entries(connection: &ProviderConnectionConfig) -> Vec<ModelCatalogEntry> {
    let rows: Vec<(&str, &str, bool)> = match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => {
            sigil_provider_deepseek::BUNDLED_DEEPSEEK_MODELS.to_vec()
        }
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses) => {
            sigil_provider_openai_responses::BUNDLED_OPENAI_RESPONSES_MODELS.to_vec()
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            sigil_provider_anthropic::BUNDLED_ANTHROPIC_MODELS.to_vec()
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            sigil_provider_gemini::BUNDLED_GEMINI_MODELS.to_vec()
        }
        _ => Vec::new(),
    };
    rows.into_iter()
        .filter_map(|(id, label, recommended)| {
            Some(ModelCatalogEntry {
                model_ref: ModelRef::new(connection.id.clone(), id).ok()?,
                display_name: label.to_owned(),
                availability: ModelAvailability::Unverified,
                recommendation: if recommended {
                    ModelRecommendation::Recommended
                } else {
                    ModelRecommendation::Standard
                },
                provenance: ModelCatalogProvenance::Bundled,
            })
        })
        .collect()
}

/// Reads only a fresh, exact-connection persistent catalog snapshot.
///
/// This never performs network discovery. Process-local credentials intentionally have no
/// persistent catalog, so environment and staged-secret routes return `None`.
#[must_use]
pub fn fresh_cached_model_entries_native(
    cache_root: &std::path::Path,
    root_config: &RootConfig,
    connection_id: &ConnectionId,
) -> Option<Vec<ModelCatalogEntry>> {
    let loaded = load_provider_connections(root_config);
    let connection = loaded.connections.get(connection_id)?;
    let fingerprint = persistent_catalog_fingerprint(&connection.config, &connection.credential)?;
    let mut cache = load_catalog_cache(cache_root, connection.config.id.as_str(), &fingerprint)?;
    if cache_age_secs(&cache) > CATALOG_FRESH_TTL_SECS {
        return None;
    }
    enrich_remote_entries(&connection.config, &mut cache.entries);
    Some(cache_entries(cache.entries))
}

#[cfg(test)]
pub(crate) fn seed_unauthenticated_catalog_cache_for_test(
    cache_root: &std::path::Path,
    connection: &ProviderConnectionConfig,
    entries: &[ModelCatalogEntry],
) -> anyhow::Result<()> {
    let loaded_ref = LoadedCredentialRef::Config(CredentialRefConfig::None);
    let resolved = ResolvedCredential {
        secret: None,
        source: ResolvedCredentialSource::None,
        generation_id: None,
    };
    let fingerprint = catalog_fingerprint(connection, &loaded_ref, &resolved);
    save_catalog_cache(cache_root, connection.id.as_str(), &fingerprint, entries)
}

fn enrich_remote_entries(connection: &ProviderConnectionConfig, entries: &mut [ModelCatalogEntry]) {
    let bundled = bundled_model_entries(connection);
    for entry in entries.iter_mut() {
        if let Some(metadata) = bundled
            .iter()
            .find(|candidate| candidate.model_ref == entry.model_ref)
        {
            entry.display_name = metadata.display_name.clone();
            entry.recommendation = metadata.recommendation;
        }
    }
    entries.sort_by(|left, right| {
        let left_rank = u8::from(left.recommendation != ModelRecommendation::Recommended);
        let right_rank = u8::from(right.recommendation != ModelRecommendation::Recommended);
        left_rank
            .cmp(&right_rank)
            .then_with(|| left.model_ref.model_id.cmp(&right.model_ref.model_id))
    });
}

fn configured_warning(
    entries: Vec<ModelCatalogEntry>,
    configured: Option<&ModelRef>,
) -> Vec<ModelCatalogEntry> {
    configured_reference(
        entries,
        configured,
        ModelAvailability::ConfiguredUnavailable,
    )
}

fn configured_reference(
    mut entries: Vec<ModelCatalogEntry>,
    configured: Option<&ModelRef>,
    availability: ModelAvailability,
) -> Vec<ModelCatalogEntry> {
    if let Some(configured) = configured
        && !entries.iter().any(|entry| entry.model_ref == *configured)
    {
        entries.push(ModelCatalogEntry {
            model_ref: configured.clone(),
            display_name: configured.model_id.clone(),
            availability,
            recommendation: ModelRecommendation::Standard,
            provenance: ModelCatalogProvenance::Configured,
        });
    }
    entries
}

fn cache_entries(mut entries: Vec<ModelCatalogEntry>) -> Vec<ModelCatalogEntry> {
    for entry in &mut entries {
        if entry.provenance == ModelCatalogProvenance::Remote {
            entry.provenance = ModelCatalogProvenance::Cache;
        }
    }
    entries
}

fn stale_reference_entries(mut entries: Vec<ModelCatalogEntry>) -> Vec<ModelCatalogEntry> {
    for entry in &mut entries {
        entry.availability = ModelAvailability::Unverified;
    }
    entries
}

fn deduplicate_entries(entries: &mut Vec<ModelCatalogEntry>) {
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.model_ref.clone()));
    entries.sort_by(|left, right| left.model_ref.model_id.cmp(&right.model_ref.model_id));
}

fn result_with(
    request: ModelCatalogRequest,
    state: ModelCatalogState,
    entries: Vec<ModelCatalogEntry>,
    retry_after_secs: Option<u64>,
) -> ModelCatalogResult {
    ModelCatalogResult {
        request_id: request.request_id,
        connection_id: request.connection_id,
        draft_revision: request.draft_revision,
        connection_fingerprint: request.connection_fingerprint,
        state,
        entries,
        retry_after_secs,
        // Catalog discovery is optional. Every provider can still accept an explicit model ID;
        // the actual request remains fail-closed when the provider rejects that route.
        manual_entry_allowed: true,
    }
}

#[must_use]
pub fn connection_semantic_fingerprint(connection: &ProviderConnectionConfig) -> String {
    let normalized_endpoint = url::Url::parse(&connection.base_url)
        .map(|mut url| {
            url.set_fragment(None);
            url.set_query(None);
            url.to_string().trim_end_matches('/').to_owned()
        })
        .unwrap_or_else(|_| connection.base_url.clone());
    stable_digest(&[
        connection.provider.as_str().as_bytes(),
        connection.protocol.as_str().as_bytes(),
        normalized_endpoint.as_bytes(),
        serde_json::to_vec(&connection.options)
            .unwrap_or_default()
            .as_slice(),
    ])
}

fn catalog_fingerprint(
    connection: &ProviderConnectionConfig,
    loaded_ref: &LoadedCredentialRef,
    credential: &ResolvedCredential,
) -> String {
    if credential.source != ResolvedCredentialSource::ProcessStaged
        && let Some(fingerprint) = persistent_catalog_fingerprint(connection, loaded_ref)
    {
        return fingerprint;
    }
    let process_secret_scope = || {
        credential
            .secret
            .as_ref()
            .map(|secret| stable_digest(&[secret.expose_secret().as_bytes()]))
            .unwrap_or_else(|| "missing".to_owned())
    };
    let credential_scope = if credential.source == ResolvedCredentialSource::ProcessStaged {
        format!("prepared-process-memory:{}", process_secret_scope())
    } else {
        match loaded_ref {
            LoadedCredentialRef::Config(
                CredentialRefConfig::Stored { .. } | CredentialRefConfig::None,
            ) => unreachable!("persistent credential refs returned above"),
            LoadedCredentialRef::Config(CredentialRefConfig::Environment { name }) => format!(
                "environment-process-memory:{name}:{}",
                process_secret_scope()
            ),
        }
    };
    let semantic_fingerprint = connection_semantic_fingerprint(connection);
    stable_digest(&[
        connection.id.as_str().as_bytes(),
        semantic_fingerprint.as_bytes(),
        credential_scope.as_bytes(),
    ])
}

fn persistent_catalog_fingerprint(
    connection: &ProviderConnectionConfig,
    loaded_ref: &LoadedCredentialRef,
) -> Option<String> {
    let credential_scope = match loaded_ref {
        LoadedCredentialRef::Config(CredentialRefConfig::Stored { id }) => {
            format!("stored:{id}")
        }
        LoadedCredentialRef::Config(CredentialRefConfig::None) => "unauthenticated".to_owned(),
        LoadedCredentialRef::Config(CredentialRefConfig::Environment { .. }) => return None,
    };
    let semantic_fingerprint = connection_semantic_fingerprint(connection);
    Some(stable_digest(&[
        connection.id.as_str().as_bytes(),
        semantic_fingerprint.as_bytes(),
        credential_scope.as_bytes(),
    ]))
}

fn credential_is_process_local(source: ResolvedCredentialSource) -> bool {
    matches!(
        source,
        ResolvedCredentialSource::Environment | ResolvedCredentialSource::ProcessStaged
    )
}

fn stable_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

fn anthropic_version(connection: &ProviderConnectionConfig) -> &str {
    connection
        .options
        .get("anthropic_version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("2023-06-01")
}

fn validate_display_name(value: &str) -> Result<(), CatalogFailure> {
    if value.trim().is_empty()
        || value.len() > 256
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{009b}'
                        | '\u{009d}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
    {
        return Err(CatalogFailure::Malformed);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(super) enum CatalogFailure {
    AuthRejected,
    Offline,
    Unsupported,
    Malformed,
    TlsRejected,
    ProtocolMismatch,
    RateLimited(Option<u64>),
}

impl CatalogFailure {
    fn state(self) -> (ModelCatalogState, Option<u64>) {
        match self {
            Self::AuthRejected => (ModelCatalogState::AuthRejected, None),
            Self::Offline => (ModelCatalogState::Offline, None),
            Self::Unsupported => (ModelCatalogState::Unsupported, None),
            Self::Malformed => (ModelCatalogState::Malformed, None),
            Self::TlsRejected => (ModelCatalogState::TlsRejected, None),
            Self::ProtocolMismatch => (ModelCatalogState::ProtocolMismatch, None),
            Self::RateLimited(retry_after) => (ModelCatalogState::RateLimited, retry_after),
        }
    }
}

fn classify_transport_failure(error: &reqwest::Error) -> CatalogFailure {
    let detail = format!("{error:#}").to_ascii_lowercase();
    if detail.contains("certificate")
        || detail.contains("tls")
        || detail.contains("unknown issuer")
        || detail.contains("invalid peer")
    {
        CatalogFailure::TlsRejected
    } else {
        CatalogFailure::Offline
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
