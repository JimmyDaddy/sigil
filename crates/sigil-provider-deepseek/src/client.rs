use std::time::Duration;

use anyhow::Result;
use reqwest::Client;

pub fn build_http_client(timeout_secs: u64) -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?)
}
