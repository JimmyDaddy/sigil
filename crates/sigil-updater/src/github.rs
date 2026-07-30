use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use reqwest::{
    Client, StatusCode,
    header::{ACCEPT, ETAG, IF_NONE_MATCH, USER_AGENT},
    redirect::Policy,
};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{
    InstallSource, Result, UpdateChannel, UpdateError,
    cache::{self, UpdateCacheEntry},
    managed_update_command,
};

const DEFAULT_OWNER: &str = "JimmyDaddy";
const DEFAULT_REPOSITORY: &str = "sigil";
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RELEASE_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ETAG_BYTES: usize = 256;

/// Integrity and immutability evidence for one exact release asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSecurity {
    pub immutable: bool,
    pub sha256: Option<String>,
    pub eligible_for_apply: bool,
    pub blocking_reason: Option<String>,
}

/// A strictly newer release selected for the requested channel and target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCandidate {
    pub version: String,
    pub tag_name: String,
    pub prerelease: bool,
    pub asset_name: Option<String>,
    pub security: ReleaseSecurity,
}

/// Result returned to CLI, TUI, and other product surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCheckOutcome {
    pub current_version: String,
    pub target: String,
    pub channel: UpdateChannel,
    pub install_source: InstallSource,
    pub checked_at_unix_seconds: u64,
    pub cached: bool,
    pub candidate: Option<UpdateCandidate>,
    pub managed_update_command: Option<String>,
}

impl UpdateCheckOutcome {
    #[must_use]
    pub fn update_available(&self) -> bool {
        self.candidate.is_some()
    }

    #[must_use]
    pub fn apply_permitted(&self) -> bool {
        self.install_source.permits_binary_replacement()
            && self
                .candidate
                .as_ref()
                .is_some_and(|candidate| candidate.security.eligible_for_apply)
    }
}

/// Inputs that determine a cache-safe update check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOptions {
    pub current_version: String,
    pub target: String,
    pub channel: UpdateChannel,
    pub install_source: InstallSource,
    pub force_refresh: bool,
}

/// GitHub release checker with a global bounded cache.
#[derive(Debug, Clone)]
pub struct UpdateService {
    client: Client,
    owner: String,
    repository: String,
    cache_file: PathBuf,
    cache_ttl: Duration,
}

impl UpdateService {
    /// Builds Sigil's production release checker.
    ///
    /// # Errors
    ///
    /// Returns an error when the hardened HTTP client cannot be constructed.
    pub fn github(cache_file: impl Into<PathBuf>) -> Result<Self> {
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(UpdateError::HttpClient)?;
        Ok(Self {
            client,
            owner: DEFAULT_OWNER.to_owned(),
            repository: DEFAULT_REPOSITORY.to_owned(),
            cache_file: cache_file.into(),
            cache_ttl: DEFAULT_CACHE_TTL,
        })
    }

    /// Checks GitHub Releases with 24-hour cache and ETag revalidation.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid versions, failed network requests, non-success
    /// GitHub responses, or malformed/oversized release payloads.
    pub async fn check(&self, options: CheckOptions) -> Result<UpdateCheckOutcome> {
        let current = Version::parse(&options.current_version).map_err(|source| {
            UpdateError::InvalidCurrentVersion {
                version: options.current_version.clone(),
                source,
            }
        })?;
        let cache_key = cache_key(&options);
        let now = unix_seconds();
        let cached = cache::load(&self.cache_file)
            .await
            .filter(|entry| entry.cache_key == cache_key);
        if !options.force_refresh
            && let Some(entry) = cached.as_ref()
            && now.saturating_sub(entry.checked_at_unix_seconds) < self.cache_ttl.as_secs()
        {
            let mut outcome = entry.outcome.clone();
            outcome.cached = true;
            return Ok(outcome);
        }

        let endpoint = format!(
            "https://api.github.com/repos/{}/{}/releases?per_page=30",
            self.owner, self.repository
        );
        let mut request = self
            .client
            .get(endpoint)
            .header(ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2026-03-10")
            .header(USER_AGENT, format!("sigil/{}", options.current_version));
        if let Some(etag) = cached.as_ref().and_then(|entry| entry.etag.as_deref()) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(UpdateError::Http)?;
        if response.status() == StatusCode::NOT_MODIFIED {
            let Some(entry) = cached else {
                return Err(UpdateError::HttpStatus(StatusCode::NOT_MODIFIED));
            };
            let mut outcome = entry.outcome;
            outcome.checked_at_unix_seconds = now;
            outcome.cached = true;
            let persisted = UpdateCacheEntry::new(cache_key, now, entry.etag, outcome.clone());
            cache::store(&self.cache_file, &persisted).await?;
            return Ok(outcome);
        }
        if !response.status().is_success() {
            return Err(UpdateError::HttpStatus(response.status()));
        }
        let etag = bounded_etag(response.headers().get(ETAG));
        let body = bounded_body(response, MAX_RELEASE_RESPONSE_BYTES).await?;
        let releases = serde_json::from_slice::<Vec<GitHubRelease>>(&body)
            .map_err(UpdateError::InvalidResponse)?;
        let candidate = select_candidate(&current, &options.target, options.channel, &releases);
        let managed_command = candidate.as_ref().and_then(|candidate| {
            managed_update_command(options.install_source, options.channel, &candidate.version)
        });
        let outcome = UpdateCheckOutcome {
            current_version: options.current_version,
            target: options.target,
            channel: options.channel,
            install_source: options.install_source,
            checked_at_unix_seconds: now,
            cached: false,
            candidate,
            managed_update_command: managed_command,
        };
        cache::store(
            &self.cache_file,
            &UpdateCacheEntry::new(cache_key, now, etag, outcome.clone()),
        )
        .await?;
        Ok(outcome)
    }

    #[must_use]
    pub fn cache_file(&self) -> &Path {
        &self.cache_file
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    immutable: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    digest: Option<String>,
}

fn select_candidate(
    current: &Version,
    target: &str,
    channel: UpdateChannel,
    releases: &[GitHubRelease],
) -> Option<UpdateCandidate> {
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.trim_start_matches('v')).ok()?;
            let is_prerelease = release.prerelease || !version.pre.is_empty();
            (version > *current && channel.accepts(current, &version, is_prerelease)).then_some((
                release,
                version,
                is_prerelease,
            ))
        })
        .max_by(|(_, left, _), (_, right, _)| left.cmp(right))
        .map(|(release, version, prerelease)| {
            let expected_asset = format!("sigil-{version}-{target}.tar.gz");
            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name == expected_asset);
            let security = release_security(release.immutable, asset);
            UpdateCandidate {
                version: version.to_string(),
                tag_name: release.tag_name.clone(),
                prerelease,
                asset_name: asset.map(|asset| asset.name.clone()),
                security,
            }
        })
}

fn release_security(immutable: bool, asset: Option<&GitHubAsset>) -> ReleaseSecurity {
    let digest = asset
        .and_then(|asset| asset.digest.as_deref())
        .and_then(valid_sha256);
    let blocking_reason = if !immutable {
        Some("GitHub release is mutable".to_owned())
    } else if asset.is_none() {
        Some("release has no exact archive for this target".to_owned())
    } else if digest.is_none() {
        Some("release asset has no valid SHA-256 digest".to_owned())
    } else {
        None
    };
    ReleaseSecurity {
        immutable,
        sha256: digest,
        eligible_for_apply: blocking_reason.is_none(),
        blocking_reason,
    }
}

fn valid_sha256(value: &str) -> Option<String> {
    let digest = value.trim().strip_prefix("sha256:")?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

fn cache_key(options: &CheckOptions) -> String {
    format!(
        "{}\0{}\0{}\0{:?}",
        options.current_version, options.target, options.channel, options.install_source
    )
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn bounded_etag(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    let value = value?.to_str().ok()?;
    (value.len() <= MAX_ETAG_BYTES && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(UpdateError::ResponseTooLarge { limit });
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpdateError::Http)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(UpdateError::ResponseTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[path = "tests/github_tests.rs"]
mod tests;
