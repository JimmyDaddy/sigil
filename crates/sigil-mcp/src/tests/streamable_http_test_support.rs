use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sigil_kernel::{
    SecretString, WebBudgetReservationKind, WebBudgetReservationRequest, WebTaskTreeBudget,
    WebTaskTreeBudgetLimits,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;

#[derive(Clone)]
pub struct FixtureResponse {
    status: u16,
    content_type: Option<&'static str>,
    body: String,
    headers: Vec<(String, String)>,
    delay: Duration,
    disconnect: bool,
}

impl FixtureResponse {
    pub fn body(status: u16, content_type: Option<&'static str>, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            disconnect: false,
        }
    }

    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: Some("application/json"),
            body: body.into(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            disconnect: false,
        }
    }

    pub fn sse(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: Some("text/event-stream"),
            body: body.into(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            disconnect: false,
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: None,
            body: String::new(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            disconnect: false,
        }
    }

    pub fn disconnect() -> Self {
        Self {
            status: 0,
            content_type: None,
            body: String::new(),
            headers: Vec::new(),
            delay: Duration::ZERO,
            disconnect: true,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub struct FixtureServer {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FixtureServer {
    pub async fn start(responses: Vec<FixtureResponse>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        tokio::spawn(async move {
            let mut responses = VecDeque::from(responses);
            while let Some(response) = responses.pop_front() {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(header_end) = find_header_end(&buffer) {
                        let content_length = parse_content_length(&buffer[..header_end]);
                        if buffer.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                    if buffer.len() > 9 * 1024 * 1024 {
                        break;
                    }
                }
                captured
                    .lock()
                    .expect("requests lock")
                    .push(String::from_utf8_lossy(&buffer).into_owned());
                tokio::time::sleep(response.delay).await;
                if response.disconnect {
                    continue;
                }
                let reason = match response.status {
                    200 => "OK",
                    202 => "Accepted",
                    307 => "Temporary Redirect",
                    405 => "Method Not Allowed",
                    _ => "Fixture",
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    reason,
                    response.body.len()
                );
                if let Some(content_type) = response.content_type {
                    head.push_str(&format!("Content-Type: {content_type}\r\n"));
                }
                for (name, value) in response.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                if socket.write_all(head.as_bytes()).await.is_err() {
                    continue;
                }
                let _ = socket.write_all(response.body.as_bytes()).await;
            }
        });
        Self { address, requests }
    }

    pub fn endpoint(&self) -> String {
        format!("http://fixture.test:{}/mcp", self.address.port())
    }
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}
fn parse_content_length(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct PlanAuthorizer {
    endpoint: String,
    profile_fingerprint: String,
    live_header_fingerprint: String,
    budget: Arc<WebTaskTreeBudget>,
    calls: Arc<AtomicUsize>,
    live_fingerprints: Arc<Mutex<Vec<String>>>,
}

impl PlanAuthorizer {
    pub fn direct(endpoint: String) -> Self {
        let live_header_fingerprint = auth::resolve_headers(
            &McpStreamableHttpHeaderConfig::default(),
            &MapHeaderEnvironment::default(),
            &Url::parse(&endpoint).expect("endpoint"),
        )
        .expect("anonymous headers")
        .live_fingerprint;
        Self::direct_with_bindings(endpoint, "fixture-profile", live_header_fingerprint)
    }

    pub fn direct_with_bindings(
        endpoint: String,
        profile_fingerprint: impl Into<String>,
        live_header_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            endpoint,
            profile_fingerprint: profile_fingerprint.into(),
            live_header_fingerprint: live_header_fingerprint.into(),
            budget: WebTaskTreeBudget::new(
                "fixture-root",
                WebTaskTreeBudgetLimits {
                    max_fetch_calls: 64,
                    max_client_search_calls: 64,
                    max_hosted_requests: 64,
                    max_network_attempts: 64,
                    max_wire_bytes: 64 * 1024 * 1024,
                    max_decoded_bytes: 64 * 1024 * 1024,
                    max_model_bytes: 64 * 1024 * 1024,
                    max_concurrent_requests: 64,
                    max_attempts_per_host: 64,
                },
                None,
            )
            .expect("budget"),
            calls: Arc::new(AtomicUsize::new(0)),
            live_fingerprints: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn call_count(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }

    pub fn live_fingerprints(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.live_fingerprints)
    }

    async fn authorize_with_fingerprint(
        &self,
        live_header_fingerprint: &str,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError> {
        self.live_fingerprints
            .lock()
            .expect("fingerprints lock")
            .push(live_header_fingerprint.to_owned());
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let attempt_id = format!("fixture-attempt-{ordinal}");
        let mut reservation = self
            .budget
            .reserve(WebBudgetReservationRequest {
                correlation_id: format!("fixture-correlation-{ordinal}"),
                attempt_id: attempt_id.clone(),
                route_lease_id: format!("fixture-lease-{ordinal}"),
                route_fingerprint: "fixture-route".to_owned(),
                kind: WebBudgetReservationKind::TransportLifecycle,
            })
            .map_err(|_| McpStreamableHttpDestinationError::BudgetExhausted)?;
        reservation
            .commit_attempt(&attempt_id, "fixture.test")
            .map_err(|_| McpStreamableHttpDestinationError::BudgetExhausted)?;
        let port = url::Url::parse(&self.endpoint)
            .expect("endpoint")
            .port()
            .expect("port");
        McpStreamableHttpAuthorizedDialPlan::direct(
            SecretString::new(self.endpoint.clone()),
            format!("http://fixture.test:{port}"),
            vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)],
            self.profile_fingerprint.clone(),
            live_header_fingerprint,
            reservation,
        )
        .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected)
    }
}

#[async_trait]
impl McpStreamableHttpDestinationAuthorizer for PlanAuthorizer {
    fn endpoint(&self) -> SecretString {
        SecretString::new(self.endpoint.clone())
    }

    fn profile_config_proxy_fingerprint(&self) -> String {
        self.profile_fingerprint.clone()
    }

    fn live_header_fingerprint(&self) -> String {
        self.live_header_fingerprint.clone()
    }

    async fn authorize_destination(
        &self,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError> {
        self.authorize_with_fingerprint(&self.live_header_fingerprint)
            .await
    }

    async fn authorize_destination_with_fingerprint(
        &self,
        live_header_fingerprint: &str,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError> {
        self.authorize_with_fingerprint(live_header_fingerprint)
            .await
    }
}

#[derive(Default)]
pub struct MapHeaderEnvironment(pub BTreeMap<String, SecretString>);

impl McpStreamableHttpHeaderEnvironment for MapHeaderEnvironment {
    fn resolve(&self, name: &str) -> Option<SecretString> {
        self.0.get(name).cloned()
    }
}
