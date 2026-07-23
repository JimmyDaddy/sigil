use std::{
    collections::BTreeMap,
    fmt,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, CompletionRequest, FrozenProviderRequestMaterial, HostedWebSearchCapability,
    ImageInputCapability, PortableTargetRequestMaterial, Provider, ProviderCapabilities,
    ProviderChunk, ProviderRequestRejection, ProviderRouteCooldownError, TaskParticipantAttemptId,
    ToolRegistry, provider_rate_limit_from_error,
};

const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(1);
const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(120);
const MAX_FALLBACK_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_DETERMINISTIC_JITTER_MS: u64 = 250;

#[derive(Clone)]
pub(crate) struct TaskProviderPressure {
    state: Arc<Mutex<ProviderPressureState>>,
    clock: Arc<dyn ProviderPressureClock>,
}

impl Default for TaskProviderPressure {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(ProviderPressureState::default())),
            clock: Arc::new(SystemProviderPressureClock),
        }
    }
}

impl fmt::Debug for TaskProviderPressure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let route_count = self
            .state
            .lock()
            .map(|state| state.routes.len())
            .unwrap_or_default();
        formatter
            .debug_struct("TaskProviderPressure")
            .field("route_count", &route_count)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct ProviderPressureState {
    routes: BTreeMap<String, ProviderRoutePressureState>,
}

struct ProviderRoutePressureState {
    cooldown_until: Instant,
    consecutive_rate_limits: u32,
    epoch: u64,
}

#[derive(Clone)]
struct ProviderRouteAdmission {
    fingerprint: String,
    epoch: u64,
}

trait ProviderPressureClock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemProviderPressureClock;

impl ProviderPressureClock for SystemProviderPressureClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

impl TaskProviderPressure {
    pub(crate) fn check(&self, provider_name: &str, model_name: &str) -> Result<()> {
        self.admit(provider_name, model_name).map(|_| ())
    }

    pub(crate) fn retry_schedule_delay(
        &self,
        provider_name: &str,
        model_name: &str,
        attempt_id: &TaskParticipantAttemptId,
    ) -> Option<(u64, String)> {
        let fingerprint = provider_route_fingerprint(provider_name, model_name);
        let now = self.clock.now();
        let state = self.state.lock().ok()?;
        let route = state.routes.get(&fingerprint)?;
        if route.cooldown_until <= now {
            return None;
        }
        let remaining = duration_millis_ceil(route.cooldown_until.duration_since(now));
        let jitter = retry_attempt_jitter_ms(&fingerprint, attempt_id.as_str());
        let maximum = u64::try_from(MAX_RATE_LIMIT_COOLDOWN.as_millis()).unwrap_or(u64::MAX);
        Some((remaining.saturating_add(jitter).min(maximum), fingerprint))
    }

    fn admit(&self, provider_name: &str, model_name: &str) -> Result<ProviderRouteAdmission> {
        let fingerprint = provider_route_fingerprint(provider_name, model_name);
        let now = self.clock.now();
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider pressure state lock poisoned"))?;
        if let Some(route) = state.routes.get(&fingerprint)
            && route.cooldown_until > now
        {
            let remaining = route.cooldown_until.duration_since(now);
            return Err(ProviderRouteCooldownError::new(
                duration_millis_ceil(remaining),
                fingerprint,
            )
            .into());
        }
        let epoch = state
            .routes
            .get(&fingerprint)
            .map_or(0, |route| route.epoch);
        Ok(ProviderRouteAdmission { fingerprint, epoch })
    }

    fn record_rate_limit(&self, admission: &ProviderRouteAdmission, retry_after_ms: Option<u64>) {
        let now = self.clock.now();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let route = state.routes.entry(admission.fingerprint.clone()).or_insert(
            ProviderRoutePressureState {
                cooldown_until: now,
                consecutive_rate_limits: 0,
                epoch: admission.epoch,
            },
        );
        route.consecutive_rate_limits = route.consecutive_rate_limits.saturating_add(1);
        route.epoch = route.epoch.saturating_add(1);
        let delay = bounded_cooldown(
            retry_after_ms,
            &admission.fingerprint,
            route.consecutive_rate_limits,
        );
        let candidate = now.checked_add(delay).unwrap_or(now);
        route.cooldown_until = route.cooldown_until.max(candidate);
    }

    fn record_success(&self, admission: &ProviderRouteAdmission) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state
            .routes
            .get(&admission.fingerprint)
            .is_some_and(|route| route.epoch == admission.epoch)
        {
            state.routes.remove(&admission.fingerprint);
        }
    }
}

pub(crate) fn wrap_task_agent_provider(
    agent: Agent<Box<dyn Provider>>,
    pressure: TaskProviderPressure,
) -> Agent<Box<dyn Provider>> {
    let (provider, tools): (Box<dyn Provider>, ToolRegistry) = agent.into_parts();
    Agent::new(
        Box::new(PressureAwareTaskProvider {
            inner: provider,
            pressure,
        }),
        tools,
    )
}

struct PressureAwareTaskProvider {
    inner: Box<dyn Provider>,
    pressure: TaskProviderPressure,
}

#[async_trait]
impl Provider for PressureAwareTaskProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn hosted_web_search_capability(&self, model_name: &str) -> HostedWebSearchCapability {
        self.inner.hosted_web_search_capability(model_name)
    }

    fn image_input_capability(&self, model_name: &str) -> ImageInputCapability {
        self.inner.image_input_capability(model_name)
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        if provider_rate_limit_from_error(error).is_some()
            || error.downcast_ref::<ProviderRouteCooldownError>().is_some()
        {
            return Some(ProviderRequestRejection::RateLimited);
        }
        self.inner.classify_pre_generation_rejection(error)
    }

    async fn prove_portable_compaction_target(
        &self,
        frozen_request: FrozenProviderRequestMaterial,
    ) -> Result<PortableTargetRequestMaterial> {
        self.inner
            .prove_portable_compaction_target(frozen_request)
            .await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let admission = self
            .pressure
            .admit(self.inner.name(), &request.model_name)?;
        let stream = match self.inner.stream(request).await {
            Ok(stream) => stream,
            Err(error) => {
                if let Some(rate_limit) = provider_rate_limit_from_error(&error) {
                    self.pressure
                        .record_rate_limit(&admission, rate_limit.retry_after_ms());
                }
                return Err(error);
            }
        };
        let pressure = self.pressure.clone();
        Ok(Box::pin(stream.map(move |item| {
            match &item {
                Ok(ProviderChunk::Done) => pressure.record_success(&admission),
                Err(error) => {
                    if let Some(rate_limit) = provider_rate_limit_from_error(error) {
                        pressure.record_rate_limit(&admission, rate_limit.retry_after_ms());
                    }
                }
                _ => {}
            }
            item
        })))
    }
}

pub(crate) fn provider_route_fingerprint(provider_name: &str, model_name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-provider-route-v1\0");
    hasher.update(provider_name.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(model_name.trim().as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn retry_attempt_jitter_ms(route_fingerprint: &str, attempt_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-provider-retry-attempt-jitter-v1\0");
    hasher.update(route_fingerprint.as_bytes());
    hasher.update(b"\0");
    hasher.update(attempt_id.as_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]) % (MAX_DETERMINISTIC_JITTER_MS + 1)
}

fn bounded_cooldown(
    retry_after_ms: Option<u64>,
    route_fingerprint: &str,
    consecutive_rate_limits: u32,
) -> Duration {
    let fallback = fallback_cooldown(route_fingerprint, consecutive_rate_limits);
    let requested = retry_after_ms
        .map(Duration::from_millis)
        .unwrap_or(fallback);
    requested.clamp(Duration::from_millis(1), MAX_RATE_LIMIT_COOLDOWN)
}

fn fallback_cooldown(route_fingerprint: &str, consecutive_rate_limits: u32) -> Duration {
    let exponent = consecutive_rate_limits.saturating_sub(1).min(5);
    let base_ms = u64::try_from(DEFAULT_RATE_LIMIT_COOLDOWN.as_millis()).unwrap_or(u64::MAX);
    let exponential_ms = base_ms
        .saturating_mul(1_u64 << exponent)
        .min(u64::try_from(MAX_FALLBACK_COOLDOWN.as_millis()).unwrap_or(u64::MAX));
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-provider-cooldown-jitter-v1\0");
    hasher.update(route_fingerprint.as_bytes());
    hasher.update(consecutive_rate_limits.to_be_bytes());
    let digest = hasher.finalize();
    let jitter_ms = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]) % (MAX_DETERMINISTIC_JITTER_MS + 1);
    Duration::from_millis(exponential_ms.saturating_add(jitter_ms))
}

fn duration_millis_ceil(duration: Duration) -> u64 {
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    if duration.subsec_nanos().is_multiple_of(1_000_000) {
        millis.max(1)
    } else {
        millis.saturating_add(1).max(1)
    }
}

#[cfg(test)]
#[path = "tests/provider_pressure_tests.rs"]
mod tests;
