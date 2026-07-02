use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeIntelStatus {
    Off,
    Starting {
        server: String,
    },
    Indexing {
        server: String,
        detail: Option<String>,
    },
    Ready {
        servers: usize,
    },
    Degraded {
        reason: String,
    },
    Error {
        reason: String,
    },
}

impl CodeIntelStatus {
    pub fn line(&self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Starting { server } => format!("starting {server}"),
            Self::Indexing { server, detail } => match detail {
                Some(detail) => format!("indexing {server} {detail}"),
                None => format!("indexing {server}"),
            },
            Self::Ready { servers } => format!("ready {servers} server(s)"),
            Self::Degraded { reason } => format!("degraded {reason}"),
            Self::Error { reason } => format!("error {reason}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QueryMetadata {
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub external_results_filtered: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelServerStatus {
    pub server: String,
    pub languages: Vec<String>,
    pub status: String,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelResponse<T> {
    pub server: String,
    pub capability: String,
    pub results: Vec<T>,
    pub metadata: QueryMetadata,
    #[serde(default)]
    pub server_statuses: Vec<CodeIntelServerStatus>,
}

pub(super) fn response<T>(
    server: String,
    languages: Vec<String>,
    capability: String,
    mut results: Vec<T>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let metadata = QueryMetadata {
        returned: total.min(limit),
        total,
        truncated,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        external_results_filtered: external_filtered,
    };
    let server_statuses = vec![server_status(
        server.clone(),
        languages,
        "ready".to_owned(),
        metadata.returned,
        metadata.total,
        metadata.truncated,
    )];
    CodeIntelResponse {
        server,
        capability,
        results,
        metadata,
        server_statuses,
    }
}

pub(super) fn response_with_filtered<T>(
    server: String,
    languages: Vec<String>,
    capability: String,
    results: Vec<T>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    response(
        server,
        languages,
        capability,
        results,
        limit,
        started,
        external_filtered,
    )
}

pub(super) fn response_with_statuses<T>(
    server: String,
    capability: String,
    mut results: Vec<T>,
    mut server_statuses: Vec<CodeIntelServerStatus>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let metadata = QueryMetadata {
        returned: total.min(limit),
        total,
        truncated,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        external_results_filtered: external_filtered,
    };
    if server_statuses.is_empty() {
        server_statuses.push(server_status(
            server.clone(),
            Vec::new(),
            "ready".to_owned(),
            metadata.returned,
            metadata.total,
            metadata.truncated,
        ));
    }
    CodeIntelResponse {
        server,
        capability,
        results,
        metadata,
        server_statuses,
    }
}

pub(super) fn server_status(
    server: String,
    languages: Vec<String>,
    status: String,
    returned: usize,
    total: usize,
    truncated: bool,
) -> CodeIntelServerStatus {
    CodeIntelServerStatus {
        server,
        languages,
        status,
        returned,
        total,
        truncated,
    }
}
