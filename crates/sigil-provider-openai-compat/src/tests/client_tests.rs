use anyhow::Result;

#[test]
fn builds_shared_provider_http_client() -> Result<()> {
    let _client = crate::client::build_http_client()?;
    Ok(())
}
