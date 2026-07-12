use std::{net::SocketAddr, sync::Arc};

use async_compression::tokio::write::{BrotliEncoder, DeflateEncoder, GzipEncoder, ZstdEncoder};
use sigil_kernel::{
    WebBudgetReservation, WebBudgetReservationKind, WebBudgetReservationRequest, WebTaskTreeBudget,
    WebTaskTreeBudgetLimits,
};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};

use super::*;

#[test]
fn dial_plan_debug_omits_exact_url_and_proxy_credentials() {
    let plan = WebFetchAuthorizedDialPlan::environment_proxy(
        Url::parse("https://example.test/path?token=secret").expect("URL should parse"),
        "https://example.test:443",
        "http://proxy.test:8080",
        SecretString::new("http://user:secret@proxy.test:8080"),
        WebFetchProxyEnvSource::HttpsProxy,
    )
    .expect("plan should be valid");

    let debug = format!("{plan:?}");
    assert!(!debug.contains("token=secret"));
    assert!(!debug.contains("user:secret"));
    assert!(debug.contains("proxy.test:8080"));
    assert_eq!(
        plan.transport_security(),
        WebFetchTransportSecurity::ProxyRemote
    );
}

#[tokio::test]
async fn fetches_pinned_plaintext_and_charges_three_byte_dimensions() {
    let body = b"hello from webfetch".to_vec();
    let (plan, mut reservation, budget) = fixture(
        "200 OK",
        &["Content-Type: text/plain; charset=utf-8"],
        body.clone(),
    )
    .await;

    let result = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::PlainText,
        )
        .await
        .expect("fetch should succeed");
    let WebFetchHopResult::Fetched(response) = result else {
        panic!("expected fetched response");
    };
    assert_eq!(response.body, "hello from webfetch");
    assert_eq!(response.wire_bytes, body.len());
    assert_eq!(response.decoded_bytes, body.len());
    assert_eq!(response.model_bytes, body.len());
    assert_eq!(
        response.transport_security,
        WebFetchTransportSecurity::DirectPinned
    );
    assert_eq!(
        response.network_guard,
        WebFetchNetworkGuard::DirectAllAddressesPinned
    );

    let snapshot = budget.snapshot().expect("budget snapshot should succeed");
    assert_eq!(snapshot.wire_bytes, body.len() as u64);
    assert_eq!(snapshot.decoded_bytes, body.len() as u64);
    assert_eq!(snapshot.model_bytes, body.len() as u64);
}

#[tokio::test]
async fn redirects_are_not_followed_and_location_is_transient() {
    let (plan, mut reservation, _) = fixture(
        "302 Found",
        &["Location: https://other.test/path?token=secret"],
        Vec::new(),
    )
    .await;

    let result = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::PlainText,
        )
        .await
        .expect("redirect should be returned to the runtime");
    let WebFetchHopResult::Redirect { status, location } = result else {
        panic!("expected redirect");
    };
    assert_eq!(status, 302);
    assert_eq!(
        location.expose_secret(),
        "https://other.test/path?token=secret"
    );
    assert_eq!(format!("{location:?}"), "SecretString([redacted])");
}

#[tokio::test]
async fn environment_proxy_route_uses_only_the_explicit_proxy() {
    let address = serve_once(
        "200 OK",
        &["Content-Type: text/plain; charset=utf-8"],
        b"proxied body".to_vec(),
        12,
    )
    .await;
    let plan = WebFetchAuthorizedDialPlan::environment_proxy(
        Url::parse("http://public.example.test/page?token=secret")
            .expect("logical URL should parse"),
        "http://public.example.test/",
        format!("http://127.0.0.1:{}/", address.port()),
        SecretString::new(format!(
            "http://proxy-user:proxy-secret@127.0.0.1:{}",
            address.port()
        )),
        WebFetchProxyEnvSource::HttpProxy,
    )
    .expect("proxy plan should build");
    let budget = test_budget();
    let mut reservation = reservation_for(&budget);
    let result = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::PlainText,
        )
        .await
        .expect("proxy fetch should succeed");
    let WebFetchHopResult::Fetched(response) = result else {
        panic!("expected fetched response");
    };
    assert_eq!(response.body, "proxied body");
    assert_eq!(
        response.transport_security,
        WebFetchTransportSecurity::ProxyRemote
    );
    let debug = format!("{plan:?}");
    assert!(!debug.contains("proxy-user"));
    assert!(!debug.contains("proxy-secret"));
    assert!(!debug.contains("token=secret"));
}

#[tokio::test]
async fn explicitly_decodes_supported_content_encodings() {
    for encoding in ["gzip", "br", "zstd", "deflate"] {
        let encoded = encode(encoding, b"bounded compressed body").await;
        let content_encoding = format!("Content-Encoding: {encoding}");
        let (plan, mut reservation, _) = fixture(
            "200 OK",
            &[
                "Content-Type: text/plain; charset=utf-8",
                content_encoding.as_str(),
            ],
            encoded,
        )
        .await;
        let result = WebFetchTransport
            .fetch_once(
                &plan,
                &mut reservation,
                WebFetchLimits::default(),
                WebFetchFormat::PlainText,
            )
            .await
            .expect("supported encoding should decode");
        let WebFetchHopResult::Fetched(response) = result else {
            panic!("expected fetched response for {encoding}");
        };
        assert_eq!(response.body, "bounded compressed body");
    }
}

#[tokio::test]
async fn rejects_compression_bombs_before_unbounded_output() {
    let encoded = encode("gzip", &vec![b'x'; 16 * 1024]).await;
    let (plan, mut reservation, _) = fixture(
        "200 OK",
        &[
            "Content-Type: text/plain; charset=utf-8",
            "Content-Encoding: gzip",
        ],
        encoded,
    )
    .await;
    let error = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits {
                max_wire_bytes: 1024,
                max_decoded_bytes: 128,
                max_model_bytes: 128,
            },
            WebFetchFormat::PlainText,
        )
        .await
        .expect_err("decoded output should be bounded");
    assert!(matches!(error, WebFetchError::DecodedLimitExceeded));
}

#[tokio::test]
async fn rejects_declared_oversize_body_before_streaming() {
    let (plan, mut reservation, budget) = fixture_with_declared_length(
        "200 OK",
        &["Content-Type: text/plain; charset=utf-8"],
        b"short".to_vec(),
        4096,
    )
    .await;
    let error = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits {
                max_wire_bytes: 32,
                max_decoded_bytes: 32,
                max_model_bytes: 32,
            },
            WebFetchFormat::PlainText,
        )
        .await
        .expect_err("declared oversize response should fail before body read");
    assert!(matches!(error, WebFetchError::ContentLengthExceeded));
    assert_eq!(
        budget
            .snapshot()
            .expect("budget snapshot should succeed")
            .wire_bytes,
        0
    );
}

#[tokio::test]
async fn decodes_bounded_non_utf8_and_rejects_malformed_charset() {
    let (plan, mut reservation, _) = fixture(
        "200 OK",
        &["Content-Type: text/plain; charset=windows-1252"],
        vec![b'c', b'a', b'f', 0xe9],
    )
    .await;
    let result = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::PlainText,
        )
        .await
        .expect("windows-1252 should decode");
    let WebFetchHopResult::Fetched(response) = result else {
        panic!("expected fetched response");
    };
    assert_eq!(response.body, "café");

    let (plan, mut reservation, _) = fixture(
        "200 OK",
        &["Content-Type: text/plain; charset=utf-8"],
        vec![0xff, 0xfe, 0xfa],
    )
    .await;
    let error = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::PlainText,
        )
        .await
        .expect_err("malformed UTF-8 should be rejected");
    assert!(matches!(error, WebFetchError::InvalidCharset));
}

#[tokio::test]
async fn extracts_hostile_html_without_active_or_terminal_control_content() {
    let html = concat!(
        "<html><head><title> Safe title </title>",
        "<meta http-equiv='refresh' content='0;url=https://evil.test'></head>",
        "<body><nav>secret navigation</nav><script>alert(1)</script>",
        "<p>Hello &amp; welcome</p><ul><li>First</li></ul>",
        "<p>\u{1b}]8;;https://evil.test\u{7}link\u{1b}]8;;\u{7}</p>",
        "</body></html>"
    );
    let (plan, mut reservation, _) = fixture(
        "200 OK",
        &["Content-Type: text/html; charset=utf-8"],
        html.as_bytes().to_vec(),
    )
    .await;
    let result = WebFetchTransport
        .fetch_once(
            &plan,
            &mut reservation,
            WebFetchLimits::default(),
            WebFetchFormat::Markdown,
        )
        .await
        .expect("HTML extraction should succeed");
    let WebFetchHopResult::Fetched(response) = result else {
        panic!("expected fetched response");
    };
    assert_eq!(response.title.as_deref(), Some(" Safe title "));
    assert!(response.body.contains("Hello & welcome"));
    assert!(response.body.contains("- First"));
    assert!(!response.body.contains("secret navigation"));
    assert!(!response.body.contains("alert(1)"));
    assert!(!response.body.contains("evil.test"));
    assert!(!response.body.contains('\u{1b}'));
    assert!(!response.body.contains('\u{7}'));
}

async fn fixture(
    status: &str,
    headers: &[&str],
    body: Vec<u8>,
) -> (
    WebFetchAuthorizedDialPlan,
    WebBudgetReservation,
    Arc<WebTaskTreeBudget>,
) {
    let declared_length = body.len();
    fixture_with_declared_length(status, headers, body, declared_length).await
}

async fn fixture_with_declared_length(
    status: &str,
    headers: &[&str],
    body: Vec<u8>,
    declared_length: usize,
) -> (
    WebFetchAuthorizedDialPlan,
    WebBudgetReservation,
    Arc<WebTaskTreeBudget>,
) {
    let address = serve_once(status, headers, body, declared_length).await;
    let url = Url::parse(&format!(
        "http://example.test:{}/page?token=secret",
        address.port()
    ))
    .expect("fixture URL should parse");
    let plan = WebFetchAuthorizedDialPlan::direct(
        url,
        format!("http://example.test:{}", address.port()),
        vec![address],
    )
    .expect("direct plan should be valid");
    let budget = test_budget();
    let reservation = reservation_for(&budget);
    (plan, reservation, budget)
}

fn reservation_for(budget: &Arc<WebTaskTreeBudget>) -> WebBudgetReservation {
    let mut reservation = budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "webfetch-correlation".to_owned(),
            attempt_id: "webfetch-attempt".to_owned(),
            route_lease_id: "webfetch-route-lease".to_owned(),
            route_fingerprint: "webfetch-route-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::FetchCall,
        })
        .expect("budget reservation should succeed");
    reservation
        .commit_call()
        .expect("logical call should commit");
    reservation
        .commit_attempt("webfetch-attempt", "example.test")
        .expect("attempt should commit");
    reservation
}

fn test_budget() -> Arc<WebTaskTreeBudget> {
    WebTaskTreeBudget::new(
        "webfetch-root",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 8,
            max_client_search_calls: 8,
            max_hosted_requests: 8,
            max_network_attempts: 16,
            max_wire_bytes: 32 * 1024 * 1024,
            max_decoded_bytes: 32 * 1024 * 1024,
            max_model_bytes: 32 * 1024 * 1024,
            max_concurrent_requests: 4,
            max_attempts_per_host: 16,
        },
        None,
    )
    .expect("test budget should be valid")
}

async fn serve_once(
    status: &str,
    headers: &[&str],
    body: Vec<u8>,
    declared_length: usize,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener
        .local_addr()
        .expect("fixture listener should have an address");
    let status = status.to_owned();
    let headers = headers.iter().map(ToString::to_string).collect::<Vec<_>>();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("fixture should accept");
        let mut request = vec![0u8; 4096];
        let _ = stream.read(&mut request).await;
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {declared_length}\r\nConnection: close\r\n"
        );
        for header in headers {
            response.push_str(&header);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        stream
            .write_all(response.as_bytes())
            .await
            .expect("fixture headers should write");
        stream
            .write_all(&body)
            .await
            .expect("fixture body should write");
        let _ = stream.shutdown().await;
    });
    address
}

async fn encode(encoding: &str, body: &[u8]) -> Vec<u8> {
    match encoding {
        "gzip" => encode_with(GzipEncoder::new(Vec::new()), body).await,
        "br" => encode_with(BrotliEncoder::new(Vec::new()), body).await,
        "zstd" => encode_with(ZstdEncoder::new(Vec::new()), body).await,
        "deflate" => encode_with(DeflateEncoder::new(Vec::new()), body).await,
        _ => panic!("unsupported fixture encoding"),
    }
}

async fn encode_with<W>(mut encoder: W, body: &[u8]) -> Vec<u8>
where
    W: AsyncWrite + Unpin + IntoTestBytes,
{
    encoder
        .write_all(body)
        .await
        .expect("fixture should encode");
    encoder.shutdown().await.expect("fixture should finish");
    encoder.into_test_bytes()
}

trait IntoTestBytes {
    fn into_test_bytes(self) -> Vec<u8>;
}

macro_rules! impl_into_test_bytes {
    ($type:ident) => {
        impl IntoTestBytes for $type<Vec<u8>> {
            fn into_test_bytes(self) -> Vec<u8> {
                self.into_inner()
            }
        }
    };
}

impl_into_test_bytes!(GzipEncoder);
impl_into_test_bytes!(BrotliEncoder);
impl_into_test_bytes!(ZstdEncoder);
impl_into_test_bytes!(DeflateEncoder);
