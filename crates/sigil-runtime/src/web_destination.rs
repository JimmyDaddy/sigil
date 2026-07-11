use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use async_trait::async_trait;
use sigil_kernel::SecretString;
use sigil_tools_builtin::{WebFetchAuthorizedDialPlan, WebFetchError, WebFetchProxyEnvSource};
use thiserror::Error;
use url::Url;

const DEFAULT_ALLOWED_PORTS: [u16; 2] = [80, 443];
const MAX_RESOLVED_ADDRESSES: usize = 64;

#[derive(Debug, Error)]
pub enum WebDestinationError {
    #[error("web destination URL is invalid: {0}")]
    InvalidUrl(&'static str),
    #[error("web destination port is not allowed")]
    PortNotAllowed,
    #[error("web destination domain is blocked")]
    BlockedDomain,
    #[error("web destination resolves to a permanently forbidden address category")]
    PermanentlyForbiddenAddress,
    #[error("web destination resolves to a private address without an exact host and CIDR grant")]
    PrivateAddressDenied,
    #[error("web destination private exception requires a complete resolved-address receipt")]
    PrivateExceptionRequiresResolvedSet,
    #[error("web destination DNS lookup failed")]
    ResolutionFailed,
    #[error("web destination DNS lookup returned no usable addresses")]
    EmptyResolution,
    #[error("web destination DNS lookup returned too many addresses")]
    ResolutionLimitExceeded,
    #[error("web proxy configuration is invalid")]
    InvalidProxy,
    #[error(transparent)]
    DialPlan(#[from] WebFetchError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    pub fn new(network: IpAddr, prefix: u8) -> Result<Self, WebDestinationError> {
        let max = if network.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(WebDestinationError::InvalidUrl("CIDR prefix is invalid"));
        }
        Ok(Self {
            network: normalize_ip(network),
            prefix,
        })
    }

    #[must_use]
    pub fn contains(self, address: IpAddr) -> bool {
        match (self.network, normalize_ip(address)) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                masked_v4(network, self.prefix) == masked_v4(address, self.prefix)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                masked_v6(network, self.prefix) == masked_v6(address, self.prefix)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebDestinationGuardPolicy {
    allowed_ports: BTreeSet<u16>,
    blocked_domains: BTreeSet<String>,
    private_exceptions: BTreeMap<String, Vec<IpCidr>>,
}

impl Default for WebDestinationGuardPolicy {
    fn default() -> Self {
        Self {
            allowed_ports: DEFAULT_ALLOWED_PORTS.into_iter().collect(),
            blocked_domains: BTreeSet::new(),
            private_exceptions: BTreeMap::new(),
        }
    }
}

impl WebDestinationGuardPolicy {
    #[must_use]
    pub fn with_allowed_ports(mut self, ports: impl IntoIterator<Item = u16>) -> Self {
        self.allowed_ports = ports.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_blocked_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.blocked_domains = domains
            .into_iter()
            .map(Into::into)
            .map(|value: String| normalize_domain_pattern(&value))
            .filter(|value| !value.is_empty())
            .collect();
        self
    }

    #[must_use]
    pub fn with_private_exception(
        mut self,
        exact_host: impl Into<String>,
        cidrs: Vec<IpCidr>,
    ) -> Self {
        let host = normalize_host(&exact_host.into());
        if !host.is_empty() && !cidrs.is_empty() {
            self.private_exceptions.insert(host, cidrs);
        }
        self
    }
}

#[derive(Clone, Default)]
pub struct ProxyEnvironment {
    http_proxy: Option<SecretString>,
    https_proxy: Option<SecretString>,
    all_proxy: Option<SecretString>,
    no_proxy: Vec<String>,
}

impl fmt::Debug for ProxyEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyEnvironment")
            .field("http_proxy_configured", &self.http_proxy.is_some())
            .field("https_proxy_configured", &self.https_proxy.is_some())
            .field("all_proxy_configured", &self.all_proxy.is_some())
            .field("no_proxy_rule_count", &self.no_proxy.len())
            .finish()
    }
}

impl ProxyEnvironment {
    #[must_use]
    pub fn from_values(
        http_proxy: Option<SecretString>,
        https_proxy: Option<SecretString>,
        all_proxy: Option<SecretString>,
        no_proxy: Option<&str>,
    ) -> Self {
        Self {
            http_proxy,
            https_proxy,
            all_proxy,
            no_proxy: no_proxy
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .take(256)
                .map(str::to_ascii_lowercase)
                .collect(),
        }
    }

    fn select_proxy(&self, url: &Url) -> Option<(&SecretString, WebFetchProxyEnvSource)> {
        let host = normalize_host(url.host_str()?);
        let port = url.port_or_known_default()?;
        if self
            .no_proxy
            .iter()
            .any(|rule| no_proxy_matches(rule, &host, port))
        {
            return None;
        }
        match url.scheme() {
            "https" => self
                .https_proxy
                .as_ref()
                .map(|proxy| (proxy, WebFetchProxyEnvSource::HttpsProxy))
                .or_else(|| {
                    self.all_proxy
                        .as_ref()
                        .map(|proxy| (proxy, WebFetchProxyEnvSource::AllProxy))
                }),
            "http" => self
                .http_proxy
                .as_ref()
                .map(|proxy| (proxy, WebFetchProxyEnvSource::HttpProxy))
                .or_else(|| {
                    self.all_proxy
                        .as_ref()
                        .map(|proxy| (proxy, WebFetchProxyEnvSource::AllProxy))
                }),
            _ => None,
        }
    }
}

#[async_trait]
pub trait WebDestinationResolver: Send + Sync {
    async fn resolve_all(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebDestinationError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWebDestinationResolver;

#[async_trait]
impl WebDestinationResolver for SystemWebDestinationResolver {
    async fn resolve_all(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebDestinationError> {
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| WebDestinationError::ResolutionFailed)?;
        Ok(addresses.map(|address| address.ip()).collect())
    }
}

#[derive(Debug, Clone)]
pub struct WebDestinationGuard<R> {
    resolver: R,
    policy: WebDestinationGuardPolicy,
    proxy_environment: ProxyEnvironment,
}

enum PreviewRoute {
    Direct,
    Proxy {
        proxy_url: SecretString,
        source: WebFetchProxyEnvSource,
    },
}

/// Pre-DNS, secret-safe projection consumed by the durable disclosure barrier.
pub struct WebDestinationPreview {
    logical_url: Url,
    host: String,
    port: u16,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: PreviewRoute,
}

impl fmt::Debug for WebDestinationPreview {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebDestinationPreview")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("safe_logical_destination", &self.safe_logical_destination)
            .field(
                "safe_transport_destination",
                &self.safe_transport_destination,
            )
            .field(
                "proxy_remote",
                &matches!(self.route, PreviewRoute::Proxy { .. }),
            )
            .finish()
    }
}

impl WebDestinationPreview {
    #[must_use]
    pub fn safe_host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn safe_logical_destination(&self) -> &str {
        &self.safe_logical_destination
    }

    #[must_use]
    pub fn safe_transport_destination(&self) -> &str {
        &self.safe_transport_destination
    }

    #[must_use]
    pub fn is_proxy_remote(&self) -> bool {
        matches!(self.route, PreviewRoute::Proxy { .. })
    }
}

impl<R> WebDestinationGuard<R>
where
    R: WebDestinationResolver,
{
    #[must_use]
    pub fn new(
        resolver: R,
        policy: WebDestinationGuardPolicy,
        proxy_environment: ProxyEnvironment,
    ) -> Self {
        Self {
            resolver,
            policy,
            proxy_environment,
        }
    }

    pub async fn authorize(
        &self,
        logical_url: Url,
    ) -> Result<WebFetchAuthorizedDialPlan, WebDestinationError> {
        let preview = self.preview(logical_url)?;
        self.authorize_preview(preview).await
    }

    pub fn preview(&self, logical_url: Url) -> Result<WebDestinationPreview, WebDestinationError> {
        let (host, port) = validate_url(&logical_url, &self.policy)?;
        validate_logical_host(&host, &self.policy)?;
        let safe_logical_destination = safe_origin(&logical_url, &host, port);
        if let Some((proxy_url, source)) = self.proxy_environment.select_proxy(&logical_url) {
            if self.policy.private_exceptions.contains_key(&host) {
                return Err(WebDestinationError::PrivateExceptionRequiresResolvedSet);
            }
            let parsed_proxy = Url::parse(proxy_url.expose_secret())
                .map_err(|_| WebDestinationError::InvalidProxy)?;
            if !matches!(parsed_proxy.scheme(), "http" | "https")
                || parsed_proxy.host_str().is_none()
            {
                return Err(WebDestinationError::InvalidProxy);
            }
            let proxy_host = normalize_host(
                parsed_proxy
                    .host_str()
                    .ok_or(WebDestinationError::InvalidProxy)?,
            );
            let proxy_port = parsed_proxy
                .port_or_known_default()
                .ok_or(WebDestinationError::InvalidProxy)?;
            let safe_transport_destination = safe_origin(&parsed_proxy, &proxy_host, proxy_port);
            return Ok(WebDestinationPreview {
                logical_url,
                host,
                port,
                safe_logical_destination,
                safe_transport_destination,
                route: PreviewRoute::Proxy {
                    proxy_url: proxy_url.clone(),
                    source,
                },
            });
        }

        Ok(WebDestinationPreview {
            logical_url,
            host,
            port,
            safe_transport_destination: safe_logical_destination.clone(),
            safe_logical_destination,
            route: PreviewRoute::Direct,
        })
    }

    pub async fn authorize_preview(
        &self,
        preview: WebDestinationPreview,
    ) -> Result<WebFetchAuthorizedDialPlan, WebDestinationError> {
        let WebDestinationPreview {
            logical_url,
            host,
            port,
            safe_logical_destination,
            safe_transport_destination,
            route,
        } = preview;
        if let PreviewRoute::Proxy { proxy_url, source } = route {
            return WebFetchAuthorizedDialPlan::environment_proxy(
                logical_url,
                safe_logical_destination,
                safe_transport_destination,
                proxy_url,
                source,
            )
            .map_err(Into::into);
        }
        let resolved = match logical_url.host() {
            Some(url::Host::Ipv4(address)) => vec![IpAddr::V4(address)],
            Some(url::Host::Ipv6(address)) => vec![IpAddr::V6(address)],
            Some(url::Host::Domain(_)) => self.resolver.resolve_all(&host, port).await?,
            None => return Err(WebDestinationError::InvalidUrl("host is required")),
        };
        let resolved = normalize_resolved_set(resolved)?;
        validate_resolved_set(&host, &resolved, &self.policy)?;
        let pinned = resolved
            .into_iter()
            .map(|address| SocketAddr::new(address, port))
            .collect();
        WebFetchAuthorizedDialPlan::direct(logical_url, safe_logical_destination, pinned)
            .map_err(Into::into)
    }
}

fn validate_url(
    url: &Url,
    policy: &WebDestinationGuardPolicy,
) -> Result<(String, u16), WebDestinationError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebDestinationError::InvalidUrl(
            "only HTTP and HTTPS are supported",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebDestinationError::InvalidUrl("userinfo is forbidden"));
    }
    let host = url
        .host_str()
        .map(normalize_host)
        .filter(|value| !value.is_empty())
        .ok_or(WebDestinationError::InvalidUrl("host is required"))?;
    let port = url
        .port_or_known_default()
        .ok_or(WebDestinationError::InvalidUrl("port is required"))?;
    if !policy.allowed_ports.contains(&port) {
        return Err(WebDestinationError::PortNotAllowed);
    }
    Ok((host, port))
}

fn validate_logical_host(
    host: &str,
    policy: &WebDestinationGuardPolicy,
) -> Result<(), WebDestinationError> {
    if is_localhost_name(host)
        || is_metadata_name(host)
        || policy
            .blocked_domains
            .iter()
            .any(|blocked| domain_matches(host, blocked))
    {
        return Err(WebDestinationError::BlockedDomain);
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        match address_category(normalize_ip(address)) {
            AddressCategory::Public => {}
            AddressCategory::Private => {
                if !policy.private_exceptions.contains_key(host) {
                    return Err(WebDestinationError::PrivateAddressDenied);
                }
            }
            AddressCategory::Permanent => {
                return Err(WebDestinationError::PermanentlyForbiddenAddress);
            }
        }
    }
    Ok(())
}

fn normalize_resolved_set(addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>, WebDestinationError> {
    if addresses.is_empty() {
        return Err(WebDestinationError::EmptyResolution);
    }
    let mut unique = BTreeSet::new();
    for address in addresses {
        unique.insert(normalize_ip(address));
        if unique.len() > MAX_RESOLVED_ADDRESSES {
            return Err(WebDestinationError::ResolutionLimitExceeded);
        }
    }
    Ok(unique.into_iter().collect())
}

fn validate_resolved_set(
    host: &str,
    addresses: &[IpAddr],
    policy: &WebDestinationGuardPolicy,
) -> Result<(), WebDestinationError> {
    if addresses
        .iter()
        .any(|address| address_category(*address) == AddressCategory::Permanent)
    {
        return Err(WebDestinationError::PermanentlyForbiddenAddress);
    }
    let private = addresses
        .iter()
        .filter(|address| address_category(**address) == AddressCategory::Private)
        .copied()
        .collect::<Vec<_>>();
    if private.is_empty() {
        return Ok(());
    }
    let cidrs = policy
        .private_exceptions
        .get(host)
        .ok_or(WebDestinationError::PrivateAddressDenied)?;
    if addresses
        .iter()
        .all(|address| cidrs.iter().any(|cidr| cidr.contains(*address)))
    {
        Ok(())
    } else {
        Err(WebDestinationError::PrivateAddressDenied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressCategory {
    Public,
    Private,
    Permanent,
}

fn address_category(address: IpAddr) -> AddressCategory {
    match normalize_ip(address) {
        IpAddr::V4(address) => {
            let octets = address.octets();
            if address.is_unspecified()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_multicast()
                || octets[0] >= 240
                || is_metadata_v4(address)
                || in_v4_prefix(address, Ipv4Addr::new(100, 64, 0, 0), 10)
                || in_v4_prefix(address, Ipv4Addr::new(192, 0, 0, 0), 24)
                || in_v4_prefix(address, Ipv4Addr::new(198, 18, 0, 0), 15)
            {
                AddressCategory::Permanent
            } else if address.is_private() {
                AddressCategory::Private
            } else {
                AddressCategory::Public
            }
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            if address.is_unspecified()
                || address.is_loopback()
                || address.is_multicast()
                || (segments[0] & 0xffc0) == 0xfe80
                || in_v6_prefix(address, Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 32)
            {
                AddressCategory::Permanent
            } else if (segments[0] & 0xfe00) == 0xfc00 {
                AddressCategory::Private
            } else {
                AddressCategory::Public
            }
        }
    }
}

fn is_metadata_v4(address: Ipv4Addr) -> bool {
    matches!(
        address.octets(),
        [169, 254, 169, 254] | [169, 254, 170, 2] | [100, 100, 100, 200]
    )
}

fn is_localhost_name(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost")
}

fn is_metadata_name(host: &str) -> bool {
    matches!(
        host,
        "metadata.google.internal"
            | "metadata.google"
            | "instance-data.ec2.internal"
            | "metadata.azure.internal"
    )
}

fn safe_origin(url: &Url, normalized_host: &str, port: u16) -> String {
    let host = if normalized_host.contains(':') {
        format!("[{normalized_host}]")
    } else {
        normalized_host.to_owned()
    };
    let default_port = match url.scheme() {
        "http" => 80,
        "https" => 443,
        _ => port,
    };
    if port == default_port {
        format!("{}://{host}/", url.scheme())
    } else {
        format!("{}://{host}:{port}/", url.scheme())
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

fn normalize_domain_pattern(value: &str) -> String {
    normalize_host(value.trim_start_matches('.'))
}

fn domain_matches(host: &str, pattern: &str) -> bool {
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

fn no_proxy_matches(rule: &str, host: &str, port: u16) -> bool {
    if rule == "*" {
        return true;
    }
    let (rule_host, rule_port) = split_no_proxy_rule(rule);
    if rule_port.is_some_and(|expected| expected != port) {
        return false;
    }
    let normalized = normalize_domain_pattern(rule_host);
    !normalized.is_empty() && domain_matches(host, &normalized)
}

fn split_no_proxy_rule(rule: &str) -> (&str, Option<u16>) {
    if let Some(bracket_end) = rule.find(']')
        && rule.starts_with('[')
    {
        let host = &rule[1..bracket_end];
        let port = rule
            .get(bracket_end + 1..)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .and_then(|value| value.parse().ok());
        return (host, port);
    }
    if rule.matches(':').count() == 1
        && let Some((host, port)) = rule.rsplit_once(':')
        && let Ok(port) = port.parse()
    {
        return (host, Some(port));
    }
    (rule, None)
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

fn masked_v4(address: Ipv4Addr, prefix: u8) -> u32 {
    let value = u32::from(address);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    value & mask
}

fn masked_v6(address: Ipv6Addr, prefix: u8) -> u128 {
    let value = u128::from(address);
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    value & mask
}

fn in_v4_prefix(address: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    masked_v4(address, prefix) == masked_v4(network, prefix)
}

fn in_v6_prefix(address: Ipv6Addr, network: Ipv6Addr, prefix: u8) -> bool {
    masked_v6(address, prefix) == masked_v6(network, prefix)
}

#[cfg(test)]
#[path = "tests/web_destination_tests.rs"]
mod tests;
