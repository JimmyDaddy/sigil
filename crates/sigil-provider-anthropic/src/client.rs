use anyhow::Result;
use reqwest::Client;

pub fn build_http_client() -> Result<Client> {
    sigil_provider_http::build_provider_http_client()
}
