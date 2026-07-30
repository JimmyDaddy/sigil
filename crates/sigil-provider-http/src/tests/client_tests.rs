use std::fs;

use anyhow::Result;
use tempfile::tempdir;

use super::*;

#[test]
fn rejects_missing_or_malformed_explicit_ca_bundle() -> Result<()> {
    let temp = tempdir()?;
    let missing = temp.path().join("missing.pem");
    assert!(build_provider_http_client_with_ca_bundle(Some(missing.as_os_str())).is_err());

    let malformed = temp.path().join("malformed.pem");
    fs::write(&malformed, "not a certificate")?;
    assert!(build_provider_http_client_with_ca_bundle(Some(malformed.as_os_str())).is_err());
    Ok(())
}

#[test]
fn default_client_keeps_builtin_roots() -> Result<()> {
    let _client = build_provider_http_client_with_ca_bundle(None)?;
    Ok(())
}
