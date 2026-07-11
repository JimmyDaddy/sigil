use super::*;

pub(super) fn build_client(
    plan: &McpStreamableHttpAuthorizedDialPlan,
) -> Result<Client, McpStreamableHttpError> {
    let endpoint = Url::parse(plan.endpoint.expose_secret())
        .map_err(|_| McpStreamableHttpError::InvalidEndpoint)?;
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .pool_max_idle_per_host(0)
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();
    match &plan.route {
        McpStreamableHttpRoute::Direct { addresses } => {
            let host = endpoint
                .host_str()
                .ok_or(McpStreamableHttpError::InvalidEndpoint)?;
            builder = builder.resolve_to_addrs(host, addresses);
        }
        McpStreamableHttpRoute::EnvironmentProxy { proxy_url } => {
            let proxy = Proxy::all(proxy_url.expose_secret())
                .map_err(|_| McpStreamableHttpError::InvalidDialPlan)?;
            builder = builder.proxy(proxy);
        }
    }
    builder
        .build()
        .map_err(|_| McpStreamableHttpError::InvalidDialPlan)
}

pub(super) fn validate_endpoint(value: &str) -> Result<Url, McpStreamableHttpError> {
    let url = Url::parse(value).map_err(|_| McpStreamableHttpError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpStreamableHttpError::InvalidEndpoint);
    }
    Ok(url)
}

pub(super) fn validate_safe_destination(value: &str) -> Result<(), McpStreamableHttpError> {
    if value.is_empty()
        || value.len() > 512
        || value.contains('?')
        || value.contains('#')
        || value.contains('@')
        || value.chars().any(char::is_control)
    {
        Err(McpStreamableHttpError::InvalidSafeDestination)
    } else {
        Ok(())
    }
}

pub(super) fn safe_origin(url: &Url) -> String {
    let host = url.host_str().unwrap_or("invalid");
    let port = url.port_or_known_default().unwrap_or_default();
    format!("{}://{host}:{port}", url.scheme())
}
