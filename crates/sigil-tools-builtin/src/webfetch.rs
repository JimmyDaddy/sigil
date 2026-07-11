use std::{
    fmt, io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use async_compression::tokio::write::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZstdDecoder};
use encoding_rs::{Encoding, UTF_8};
use futures::StreamExt;
use reqwest::{
    Client, Proxy, StatusCode,
    header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use sigil_kernel::{SecretString, WebBudgetByteKind, WebBudgetError, WebBudgetReservation};
use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use url::Url;

const MAX_RESPONSE_HEADERS: usize = 128;
const MAX_RESPONSE_HEADER_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchRoute {
    Direct,
    EnvironmentProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchNetworkGuard {
    DirectAllAddressesPinned,
    ProxyLogicalDestinationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchTransportSecurity {
    DirectPinned,
    ProxyRemote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchProxyEnvSource {
    HttpProxy,
    HttpsProxy,
    AllProxy,
}

/// Runtime-authorized, single-hop network plan. Exact URLs and proxy credentials remain transient.
pub struct WebFetchAuthorizedDialPlan {
    logical_url: Url,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: WebFetchRoute,
    guard: WebFetchNetworkGuard,
    security: WebFetchTransportSecurity,
    direct_addresses: Vec<SocketAddr>,
    proxy_url: Option<SecretString>,
    proxy_env_source: Option<WebFetchProxyEnvSource>,
}

impl fmt::Debug for WebFetchAuthorizedDialPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebFetchAuthorizedDialPlan")
            .field("safe_logical_destination", &self.safe_logical_destination)
            .field(
                "safe_transport_destination",
                &self.safe_transport_destination,
            )
            .field("route", &self.route)
            .field("guard", &self.guard)
            .field("security", &self.security)
            .field("direct_address_count", &self.direct_addresses.len())
            .field("proxy_env_source", &self.proxy_env_source)
            .finish()
    }
}

impl WebFetchAuthorizedDialPlan {
    pub fn direct(
        logical_url: Url,
        safe_logical_destination: impl Into<String>,
        direct_addresses: Vec<SocketAddr>,
    ) -> Result<Self, WebFetchError> {
        validate_logical_url(&logical_url)?;
        if direct_addresses.is_empty() {
            return Err(WebFetchError::InvalidDialPlan(
                "direct route requires at least one pinned address".to_owned(),
            ));
        }
        let safe_logical_destination = safe_logical_destination.into();
        validate_safe_destination(&safe_logical_destination)?;
        Ok(Self {
            logical_url,
            safe_transport_destination: safe_logical_destination.clone(),
            safe_logical_destination,
            route: WebFetchRoute::Direct,
            guard: WebFetchNetworkGuard::DirectAllAddressesPinned,
            security: WebFetchTransportSecurity::DirectPinned,
            direct_addresses,
            proxy_url: None,
            proxy_env_source: None,
        })
    }

    pub fn environment_proxy(
        logical_url: Url,
        safe_logical_destination: impl Into<String>,
        safe_transport_destination: impl Into<String>,
        proxy_url: SecretString,
        proxy_env_source: WebFetchProxyEnvSource,
    ) -> Result<Self, WebFetchError> {
        validate_logical_url(&logical_url)?;
        let safe_logical_destination = safe_logical_destination.into();
        let safe_transport_destination = safe_transport_destination.into();
        validate_safe_destination(&safe_logical_destination)?;
        validate_safe_destination(&safe_transport_destination)?;
        Url::parse(proxy_url.expose_secret())
            .map_err(|_| WebFetchError::InvalidDialPlan("proxy URL is invalid".to_owned()))?;
        Ok(Self {
            logical_url,
            safe_logical_destination,
            safe_transport_destination,
            route: WebFetchRoute::EnvironmentProxy,
            guard: WebFetchNetworkGuard::ProxyLogicalDestinationOnly,
            security: WebFetchTransportSecurity::ProxyRemote,
            direct_addresses: Vec::new(),
            proxy_url: Some(proxy_url),
            proxy_env_source: Some(proxy_env_source),
        })
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
    pub fn route(&self) -> WebFetchRoute {
        self.route
    }

    #[must_use]
    pub fn network_guard(&self) -> WebFetchNetworkGuard {
        self.guard
    }

    #[must_use]
    pub fn transport_security(&self) -> WebFetchTransportSecurity {
        self.security
    }

    #[must_use]
    pub fn logical_url(&self) -> &Url {
        &self.logical_url
    }

    /// Returns the complete runtime-validated address set for a direct route.
    ///
    /// The slice is empty for proxy routes. Consumers must preserve the route and guard evidence;
    /// they must not resolve the logical host again or reinterpret an empty slice as direct.
    #[must_use]
    pub fn direct_addresses(&self) -> &[SocketAddr] {
        &self.direct_addresses
    }

    /// Returns the transient authorized proxy URL for a proxy route.
    ///
    /// The secret carrier has redacted `Debug` and no serialization implementation.
    #[must_use]
    pub fn proxy_url(&self) -> Option<&SecretString> {
        self.proxy_url.as_ref()
    }

    #[must_use]
    pub fn proxy_env_source(&self) -> Option<WebFetchProxyEnvSource> {
        self.proxy_env_source
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebFetchLimits {
    pub max_wire_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_model_bytes: usize,
}

impl Default for WebFetchLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 4 * 1024 * 1024,
            max_decoded_bytes: 8 * 1024 * 1024,
            max_model_bytes: 256 * 1024,
        }
    }
}

impl WebFetchLimits {
    fn validate(self) -> Result<Self, WebFetchError> {
        if self.max_wire_bytes == 0 || self.max_decoded_bytes == 0 || self.max_model_bytes == 0 {
            return Err(WebFetchError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchFormat {
    PlainText,
    Markdown,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WebFetchFetchedResponse {
    pub status: u16,
    pub body: String,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub wire_bytes: usize,
    pub decoded_bytes: usize,
    pub model_bytes: usize,
    pub truncated: bool,
    pub transport_security: WebFetchTransportSecurity,
    pub network_guard: WebFetchNetworkGuard,
}

impl fmt::Debug for WebFetchFetchedResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebFetchFetchedResponse")
            .field("status", &self.status)
            .field("content_type_present", &self.content_type.is_some())
            .field("title_present", &self.title.is_some())
            .field("wire_bytes", &self.wire_bytes)
            .field("decoded_bytes", &self.decoded_bytes)
            .field("model_bytes", &self.model_bytes)
            .field("truncated", &self.truncated)
            .field("transport_security", &self.transport_security)
            .field("network_guard", &self.network_guard)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WebFetchHopResult {
    Fetched(WebFetchFetchedResponse),
    Redirect { status: u16, location: SecretString },
}

#[derive(Debug, Error)]
pub enum WebFetchError {
    #[error("webfetch logical URL is invalid: {0}")]
    InvalidUrl(&'static str),
    #[error("webfetch authorized dial plan is invalid: {0}")]
    InvalidDialPlan(String),
    #[error("webfetch limits must all be non-zero")]
    InvalidLimits,
    #[error("webfetch response headers exceed the bounded limit")]
    HeaderLimitExceeded,
    #[error("webfetch response content length exceeds the wire-byte limit")]
    ContentLengthExceeded,
    #[error("webfetch wire body exceeds the configured limit")]
    WireLimitExceeded,
    #[error("webfetch decoded body exceeds the configured limit")]
    DecodedLimitExceeded,
    #[error("webfetch redirect response is missing one valid Location header")]
    InvalidRedirect,
    #[error("webfetch content encoding is unsupported")]
    UnsupportedContentEncoding,
    #[error("webfetch response charset is unsupported or malformed")]
    InvalidCharset,
    #[error("webfetch response media type is unsupported")]
    UnsupportedContentType,
    #[error("webfetch request failed")]
    Request,
    #[error("webfetch client construction failed")]
    ClientBuild,
    #[error("webfetch decode failed")]
    Decode(#[source] io::Error),
    #[error(transparent)]
    Budget(#[from] WebBudgetError),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WebFetchTransport;

impl WebFetchTransport {
    pub async fn fetch_once(
        &self,
        plan: &WebFetchAuthorizedDialPlan,
        reservation: &mut WebBudgetReservation,
        limits: WebFetchLimits,
        format: WebFetchFormat,
    ) -> Result<WebFetchHopResult, WebFetchError> {
        let limits = limits.validate()?;
        let client = build_client(plan)?;
        let response = client
            .get(plan.logical_url.clone())
            .header(reqwest::header::ACCEPT, "text/html,text/plain;q=0.9")
            .send()
            .await
            .map_err(|_| WebFetchError::Request)?;

        validate_response_headers(response.headers())?;
        if let Some(content_length) = response.headers().get(CONTENT_LENGTH) {
            let length = content_length
                .to_str()
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or(WebFetchError::HeaderLimitExceeded)?;
            if length > limits.max_wire_bytes {
                return Err(WebFetchError::ContentLengthExceeded);
            }
        }

        if response.status().is_redirection() {
            return redirect_result(response.status(), response.headers());
        }

        let status = response.status().as_u16();
        let content_type = bounded_header(response.headers(), CONTENT_TYPE)?;
        ensure_supported_media_type(content_type.as_deref())?;
        let content_encoding = bounded_header(response.headers(), CONTENT_ENCODING)?;
        let wire = read_wire_body(response, reservation, limits.max_wire_bytes).await?;
        let decoded =
            decode_body(&wire, content_encoding.as_deref(), limits.max_decoded_bytes).await?;
        reservation.charge_chunk(WebBudgetByteKind::Decoded, usize_to_u64(decoded.len()))?;

        let decoded_text = decode_charset(&decoded, content_type.as_deref())?;
        let (body, title) = normalize_body(&decoded_text, content_type.as_deref(), format);
        let (body, truncated) = truncate_utf8(body, limits.max_model_bytes);
        reservation.charge_chunk(WebBudgetByteKind::Model, usize_to_u64(body.len()))?;

        Ok(WebFetchHopResult::Fetched(WebFetchFetchedResponse {
            status,
            wire_bytes: wire.len(),
            decoded_bytes: decoded.len(),
            model_bytes: body.len(),
            body,
            content_type,
            title,
            truncated,
            transport_security: plan.security,
            network_guard: plan.guard,
        }))
    }
}

fn build_client(plan: &WebFetchAuthorizedDialPlan) -> Result<Client, WebFetchError> {
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();
    match plan.route {
        WebFetchRoute::Direct => {
            let host = plan
                .logical_url
                .host_str()
                .ok_or(WebFetchError::InvalidUrl("host is required"))?;
            builder = builder.resolve_to_addrs(host, &plan.direct_addresses);
        }
        WebFetchRoute::EnvironmentProxy => {
            let proxy_url = plan.proxy_url.as_ref().ok_or_else(|| {
                WebFetchError::InvalidDialPlan("proxy route is missing a proxy URL".to_owned())
            })?;
            let proxy =
                Proxy::all(proxy_url.expose_secret()).map_err(|_| WebFetchError::ClientBuild)?;
            builder = builder.proxy(proxy);
        }
    }
    builder.build().map_err(|_| WebFetchError::ClientBuild)
}

fn validate_logical_url(url: &Url) -> Result<(), WebFetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebFetchError::InvalidUrl(
            "only HTTP and HTTPS are supported",
        ));
    }
    if url.host_str().is_none() {
        return Err(WebFetchError::InvalidUrl("host is required"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebFetchError::InvalidUrl("userinfo is forbidden"));
    }
    Ok(())
}

fn validate_safe_destination(value: &str) -> Result<(), WebFetchError> {
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(|character| character.is_control())
        || value.contains('@')
        || value.contains('?')
        || value.contains('#')
    {
        return Err(WebFetchError::InvalidDialPlan(
            "safe destination projection is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_response_headers(headers: &reqwest::header::HeaderMap) -> Result<(), WebFetchError> {
    if headers.len() > MAX_RESPONSE_HEADERS {
        return Err(WebFetchError::HeaderLimitExceeded);
    }
    let total = headers.iter().try_fold(0usize, |total, (name, value)| {
        total
            .checked_add(name.as_str().len())
            .and_then(|value_total| value_total.checked_add(value.as_bytes().len()))
            .ok_or(WebFetchError::HeaderLimitExceeded)
    })?;
    if total > MAX_RESPONSE_HEADER_BYTES {
        return Err(WebFetchError::HeaderLimitExceeded);
    }
    Ok(())
}

fn bounded_header(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Result<Option<String>, WebFetchError> {
    let values = headers.get_all(&name);
    if values.iter().count() > 1 {
        return Err(WebFetchError::HeaderLimitExceeded);
    }
    values
        .iter()
        .next()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| WebFetchError::HeaderLimitExceeded)
        })
        .transpose()
}

fn redirect_result(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Result<WebFetchHopResult, WebFetchError> {
    let values = headers.get_all(LOCATION);
    if values.iter().count() != 1 {
        return Err(WebFetchError::InvalidRedirect);
    }
    let location = values
        .iter()
        .next()
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 8 * 1024)
        .ok_or(WebFetchError::InvalidRedirect)?;
    Ok(WebFetchHopResult::Redirect {
        status: status.as_u16(),
        location: SecretString::new(location),
    })
}

async fn read_wire_body(
    response: reqwest::Response,
    reservation: &mut WebBudgetReservation,
    limit: usize,
) -> Result<Vec<u8>, WebFetchError> {
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| WebFetchError::Request)?;
        let next = output
            .len()
            .checked_add(chunk.len())
            .ok_or(WebFetchError::WireLimitExceeded)?;
        if next > limit {
            return Err(WebFetchError::WireLimitExceeded);
        }
        reservation.charge_chunk(WebBudgetByteKind::Wire, usize_to_u64(chunk.len()))?;
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

async fn decode_body(
    wire: &[u8],
    content_encoding: Option<&str>,
    limit: usize,
) -> Result<Vec<u8>, WebFetchError> {
    let encoding = content_encoding
        .unwrap_or("identity")
        .trim()
        .to_ascii_lowercase();
    if encoding.contains(',') {
        return Err(WebFetchError::UnsupportedContentEncoding);
    }
    match encoding.as_str() {
        "" | "identity" => {
            if wire.len() > limit {
                return Err(WebFetchError::DecodedLimitExceeded);
            }
            Ok(wire.to_vec())
        }
        "gzip" | "x-gzip" => decode_with_writer(GzipDecoder::new, wire, limit).await,
        "br" => decode_with_writer(BrotliDecoder::new, wire, limit).await,
        "zstd" => decode_with_writer(ZstdDecoder::new, wire, limit).await,
        "deflate" => decode_with_writer(DeflateDecoder::new, wire, limit).await,
        _ => Err(WebFetchError::UnsupportedContentEncoding),
    }
}

async fn decode_with_writer<D, F>(
    constructor: F,
    wire: &[u8],
    limit: usize,
) -> Result<Vec<u8>, WebFetchError>
where
    D: AsyncWrite + Unpin,
    F: FnOnce(BoundedAsyncWriter) -> D,
    D: IntoBoundedWriter,
{
    let mut decoder = constructor(BoundedAsyncWriter::new(limit));
    decoder.write_all(wire).await.map_err(map_decode_error)?;
    decoder.shutdown().await.map_err(map_decode_error)?;
    Ok(decoder.into_bounded_writer().into_inner())
}

trait IntoBoundedWriter {
    fn into_bounded_writer(self) -> BoundedAsyncWriter;
}

macro_rules! impl_into_bounded_writer {
    ($type:ident) => {
        impl IntoBoundedWriter for $type<BoundedAsyncWriter> {
            fn into_bounded_writer(self) -> BoundedAsyncWriter {
                self.into_inner()
            }
        }
    };
}

impl_into_bounded_writer!(GzipDecoder);
impl_into_bounded_writer!(BrotliDecoder);
impl_into_bounded_writer!(ZstdDecoder);
impl_into_bounded_writer!(DeflateDecoder);

fn map_decode_error(error: io::Error) -> WebFetchError {
    if error.kind() == io::ErrorKind::FileTooLarge {
        WebFetchError::DecodedLimitExceeded
    } else {
        WebFetchError::Decode(error)
    }
}

struct BoundedAsyncWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedAsyncWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl AsyncWrite for BoundedAsyncWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let next = match self.bytes.len().checked_add(buffer.len()) {
            Some(next) => next,
            None => return Poll::Ready(Err(io::Error::from(io::ErrorKind::FileTooLarge))),
        };
        if next > self.limit {
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::FileTooLarge)));
        }
        self.bytes.extend_from_slice(buffer);
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

fn ensure_supported_media_type(content_type: Option<&str>) -> Result<(), WebFetchError> {
    let Some(content_type) = content_type else {
        return Ok(());
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media_type == "text/plain"
        || media_type == "text/html"
        || media_type == "application/xhtml+xml"
    {
        Ok(())
    } else {
        Err(WebFetchError::UnsupportedContentType)
    }
}

fn decode_charset(bytes: &[u8], content_type: Option<&str>) -> Result<String, WebFetchError> {
    let header_encoding = content_type
        .and_then(parse_charset)
        .map(|label| Encoding::for_label(label.as_bytes()))
        .transpose_option()
        .ok_or(WebFetchError::InvalidCharset)?;
    let encoding = Encoding::for_bom(bytes)
        .map(|(encoding, _)| encoding)
        .or(header_encoding)
        .unwrap_or(UTF_8);
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        return Err(WebFetchError::InvalidCharset);
    }
    Ok(decoded.into_owned())
}

trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}

impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            Some(Some(value)) => Some(Some(value)),
            Some(None) => None,
            None => Some(None),
        }
    }
}

fn parse_charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        let label = value.trim().trim_matches(['\"', '\'']).to_ascii_lowercase();
        (!label.is_empty() && label.len() <= 64).then_some(label)
    })
}

fn normalize_body(
    decoded: &str,
    content_type: Option<&str>,
    format: WebFetchFormat,
) -> (String, Option<String>) {
    let is_html = content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| {
            value.trim().eq_ignore_ascii_case("text/html")
                || value.trim().eq_ignore_ascii_case("application/xhtml+xml")
        });
    let (body, title) = if is_html {
        extract_html(decoded, format)
    } else {
        (decoded.to_owned(), None)
    };
    let terminal_safe = strip_terminal_sequences(&body);
    (sigil_kernel::safe_persistence_text(&terminal_safe), title)
}

fn strip_terminal_sequences(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b']') {
            index += 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    index += 1;
                    break;
                }
                if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    break;
                }
            }
            continue;
        }
        if bytes[index] < 0x20 && !matches!(bytes[index], b'\n' | b'\r' | b'\t') {
            index += 1;
            continue;
        }
        let character_length = value[index..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        output.extend_from_slice(&bytes[index..index + character_length]);
        index += character_length;
    }
    String::from_utf8(output).unwrap_or_default()
}

fn extract_html(html: &str, format: WebFetchFormat) -> (String, Option<String>) {
    let lower = html.to_ascii_lowercase();
    let title = extract_tag_text(html, &lower, "title")
        .map(|value| sigil_kernel::safe_persistence_text(&decode_basic_entities(&value)))
        .filter(|value| !value.trim().is_empty());
    let mut output = String::with_capacity(html.len().min(256 * 1024));
    let mut index = 0;
    let mut suppressed: Option<&str> = None;
    while index < html.len() {
        let Some(relative_open) = html[index..].find('<') else {
            if suppressed.is_none() {
                output.push_str(&html[index..]);
            }
            break;
        };
        let open = index + relative_open;
        if suppressed.is_none() {
            output.push_str(&html[index..open]);
        }
        let Some(relative_close) = html[open..].find('>') else {
            break;
        };
        let close = open + relative_close + 1;
        let tag = lower[open + 1..close - 1].trim();
        let closing = tag.starts_with('/');
        let tag_name = tag
            .trim_start_matches('/')
            .split(|character: char| character.is_ascii_whitespace() || character == '/')
            .next()
            .unwrap_or_default();
        if let Some(active) = suppressed {
            if closing && tag_name == active {
                suppressed = None;
            }
        } else if !closing
            && matches!(
                tag_name,
                "script" | "style" | "nav" | "noscript" | "svg" | "head" | "iframe"
            )
        {
            suppressed = Some(tag_name);
        } else if matches!(tag_name, "p" | "div" | "section" | "article" | "br" | "li") {
            output.push('\n');
            if format == WebFetchFormat::Markdown && tag_name == "li" && !closing {
                output.push_str("- ");
            }
        }
        index = close;
    }
    let decoded = decode_basic_entities(&output);
    (collapse_whitespace(&decoded), title)
}

fn extract_tag_text(html: &str, lower: &str, tag: &str) -> Option<String> {
    let open_pattern = format!("<{tag}");
    let open = lower.find(&open_pattern)?;
    let content_start = open + lower[open..].find('>')? + 1;
    let close_pattern = format!("</{tag}>");
    let content_end = content_start + lower[content_start..].find(&close_pattern)?;
    Some(html[content_start..content_end].to_owned())
}

fn decode_basic_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn collapse_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut blank_lines = 0usize;
    for line in value.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
    }
    output.trim().to_owned()
}

fn truncate_utf8(mut value: String, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value, false);
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    (value, true)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/webfetch_tests.rs"]
mod tests;
