use anyhow::Result;
use reqwest::Client;

pub fn build_http_client() -> Result<Client> {
    Ok(Client::builder().build()?)
}
