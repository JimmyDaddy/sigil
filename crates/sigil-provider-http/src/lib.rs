use std::{env, ffi::OsStr, fs, path::Path};

use anyhow::{Context, Result};
use reqwest::{Certificate, Client};

/// Builds the common provider HTTP client.
///
/// rustls intentionally keeps its audited built-in roots. When `SSL_CERT_FILE`
/// is explicitly set, each PEM certificate in that bundle is appended as an
/// additional trust anchor for private enterprise gateways. Certificate-chain
/// and hostname verification remain enabled.
pub fn build_provider_http_client() -> Result<Client> {
    build_provider_http_client_with_ca_bundle(env::var_os("SSL_CERT_FILE").as_deref())
}

fn build_provider_http_client_with_ca_bundle(ca_bundle: Option<&OsStr>) -> Result<Client> {
    let mut builder = Client::builder();
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
