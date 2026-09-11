use std::{env, ffi::OsStr, fs, path::Path, time::Duration};

use anyhow::{Context, Result};
use reqwest::{Certificate, Client};

/// Redirect behavior for a provider HTTP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHttpRedirectPolicy {
    /// Use reqwest's default bounded redirect policy.
    Default,
    /// Do not follow redirects.
    None,
    /// Follow at most this many redirects.
    Limited(usize),
}

/// Transport-only options shared by provider and provider-adjacent HTTP clients.
///
/// Provider protocol, authentication, and response semantics remain owned by their callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderHttpClientOptions {
    pub timeout: Option<Duration>,
    pub redirect: ProviderHttpRedirectPolicy,
    pub referer: bool,
}

impl Default for ProviderHttpClientOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            redirect: ProviderHttpRedirectPolicy::Default,
            referer: true,
        }
    }
}

/// Builds the common provider HTTP client.
///
/// rustls intentionally keeps its audited built-in roots. When `SSL_CERT_FILE`
/// is explicitly set, each PEM certificate in that bundle is appended as an
/// additional trust anchor for private enterprise gateways. Certificate-chain
/// and hostname verification remain enabled.
pub fn build_provider_http_client() -> Result<Client> {
    build_provider_http_client_with_options(ProviderHttpClientOptions::default())
}

/// Builds a client with transport-specific timeout and redirect settings while retaining the
/// shared CA-bundle and TLS verification behavior.
pub fn build_provider_http_client_with_options(
    options: ProviderHttpClientOptions,
) -> Result<Client> {
    build_provider_http_client_with_ca_bundle(env::var_os("SSL_CERT_FILE").as_deref(), options)
}

fn build_provider_http_client_with_ca_bundle(
    ca_bundle: Option<&OsStr>,
    options: ProviderHttpClientOptions,
) -> Result<Client> {
    let mut builder = Client::builder();
    if let Some(timeout) = options.timeout {
        builder = builder.timeout(timeout);
    }
    builder = match options.redirect {
        ProviderHttpRedirectPolicy::Default => builder,
        ProviderHttpRedirectPolicy::None => builder.redirect(reqwest::redirect::Policy::none()),
        ProviderHttpRedirectPolicy::Limited(limit) => {
            builder.redirect(reqwest::redirect::Policy::limited(limit))
        }
    };
    if !options.referer {
        builder = builder.referer(false);
    }
    if let Some(path) = ca_bundle {
        let certificates = load_ca_bundle(Path::new(path))?;
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .context("failed to build provider HTTP client")
}

fn load_ca_bundle(path: &Path) -> Result<Vec<Certificate>> {
    let pem = fs::read(path)
        .with_context(|| format!("failed to read SSL_CERT_FILE {}", path.display()))?;
    let certificates = Certificate::from_pem_bundle(&pem)
        .with_context(|| format!("failed to parse SSL_CERT_FILE {}", path.display()))?;
    anyhow::ensure!(
        !certificates.is_empty(),
        "SSL_CERT_FILE {} contains no PEM certificates",
        path.display()
    );
    Ok(certificates)
}

#[cfg(test)]
#[path = "tests/client_tests.rs"]
mod client_tests;
