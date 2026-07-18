use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    LocalSessionCatalogState, SessionCatalogProjectionEntry, SessionCatalogProjectionError,
    SessionCatalogProjectionService, SessionCatalogWorkspaceMetadata, catalog_state_name,
    decode_entry_row, to_i64, workspace_metadata,
};

pub const DEFAULT_SESSION_CATALOG_PAGE_SIZE: usize = 50;
pub const MAX_SESSION_CATALOG_PAGE_SIZE: usize = 100;
const SESSION_CATALOG_QUERY_SCHEMA_VERSION: u16 = 1;
const SESSION_CATALOG_SEARCH_MAX_BYTES: usize = 160;
const SESSION_CATALOG_PROVIDER_MAX_BYTES: usize = 128;

/// Bounded filter and keyset request for one workspace historical session page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionQuery {
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_state: Option<LocalSessionCatalogState>,
}

impl Default for SessionCatalogProjectionQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_SESSION_CATALOG_PAGE_SIZE,
            cursor: None,
            search: None,
            provider_name: None,
            pinned: None,
            source_state: None,
        }
    }
}

/// One generation-consistent keyset page from the rebuildable historical catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionPage {
    pub workspace_id: String,
    pub generation: u64,
    pub reconciled_at_unix_ms: u64,
    pub degraded_source_count: usize,
    pub identity_conflict_count: usize,
    pub truncated_source_count: usize,
    pub entries: Vec<SessionCatalogProjectionEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct SessionCatalogPageCursorV1 {
    schema_version: u16,
    generation: u64,
    filter_sha256: String,
    last_modified_at_unix_ms: u64,
    last_session_id: String,
    last_session_ref: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct NormalizedSessionCatalogFilter<'a> {
    schema_version: u16,
    workspace_id: &'a str,
    search: Option<&'a str>,
    provider_name: Option<&'a str>,
    pinned: Option<bool>,
    source_state: Option<&'static str>,
    sort: &'static str,
}

struct ValidatedSessionCatalogQuery {
    limit: usize,
    cursor: Option<SessionCatalogPageCursorV1>,
    search_pattern: Option<String>,
    provider_name: Option<String>,
    pinned: Option<bool>,
    source_state: Option<LocalSessionCatalogState>,
    filter_sha256: String,
}

impl SessionCatalogProjectionService {
    /// Queries one generation-consistent historical page without reconciling durable sources.
    ///
    /// Callers that require latest durable history should call [`Self::reconcile`] first. Active
    /// run, approval and progress state are intentionally absent from this projection page.
    ///
    /// # Errors
    ///
    /// Returns a typed error when filters/cursor are invalid, the cursor generation is stale, the
    /// database schema is unavailable, or a stored row cannot be decoded.
    pub fn query(
        &self,
        query: SessionCatalogProjectionQuery,
    ) -> Result<SessionCatalogProjectionPage, SessionCatalogProjectionError> {
        let validated = validate_query(&self.lifecycle.workspace_id, query)?;
        let mut connection = self.open_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let metadata = workspace_metadata(&transaction, &self.lifecycle.workspace_id)?;
        let generation = metadata.as_ref().map_or(0, |metadata| metadata.generation);
        if let Some(cursor) = &validated.cursor
            && cursor.generation != generation
        {
            return Err(SessionCatalogProjectionError::StaleCursor {
                cursor_generation: cursor.generation,
                current_generation: generation,
            });
        }
        let cursor_modified = validated
            .cursor
            .as_ref()
            .map(|cursor| to_i64(cursor.last_modified_at_unix_ms, "cursor modified time"))
            .transpose()?;
        let cursor_session_id = validated
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_session_id.as_str());
        let cursor_session_ref = validated
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_session_ref.as_str());
        let pinned = validated.pinned.map(i64::from);
        let source_state = validated.source_state.map(catalog_state_name);
        let requested_rows =
            validated
                .limit
                .checked_add(1)
                .ok_or(SessionCatalogProjectionError::InvalidQuery {
                    message: "page limit overflowed".to_owned(),
                })?;
        let mut statement = transaction.prepare(
            "SELECT workspace_id, session_ref, session_id, source_state, source_bytes, \
             source_modified_at_unix_ms, source_content_sha256, first_stream_sequence, \
             last_stream_sequence, last_event_id, last_record_checksum, provider_name, model_name, \
             title, user_message_count, assistant_message_count, tool_result_count, \
             control_entry_count, latest_usage_json, latest_task_json, latest_readiness_json, \
             pinned, indexed_at_unix_ms \
             FROM session_catalog_entry_v1 \
             WHERE workspace_id = ?1 \
               AND (?2 IS NULL OR provider_name = ?2) \
               AND (?3 IS NULL OR pinned = ?3) \
               AND (?4 IS NULL OR source_state = ?4) \
               AND (?5 IS NULL OR title_search LIKE ?5 ESCAPE '\\') \
               AND (\
                    ?6 IS NULL \
                    OR source_modified_at_unix_ms < ?6 \
                    OR (source_modified_at_unix_ms = ?6 \
                        AND COALESCE(session_id, '') < ?7) \
                    OR (source_modified_at_unix_ms = ?6 \
                        AND COALESCE(session_id, '') = ?7 \
                        AND session_ref < ?8)\
               ) \
             ORDER BY source_modified_at_unix_ms DESC, COALESCE(session_id, '') DESC, \
                      session_ref DESC \
             LIMIT ?9",
        )?;
        let rows = statement.query_map(
            params![
                self.lifecycle.workspace_id,
                validated.provider_name,
                pinned,
                source_state,
                validated.search_pattern,
                cursor_modified,
                cursor_session_id,
                cursor_session_ref,
                i64::try_from(requested_rows).map_err(|_| {
                    SessionCatalogProjectionError::InvalidQuery {
                        message: "page limit exceeds SQLite integer range".to_owned(),
                    }
                })?,
            ],
            decode_entry_row,
        )?;
        let mut entries = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = entries.len() > validated.limit;
        entries.truncate(validated.limit);
        let next_cursor = if has_more {
            entries
                .last()
                .map(|entry| {
                    encode_cursor(&SessionCatalogPageCursorV1 {
                        schema_version: SESSION_CATALOG_QUERY_SCHEMA_VERSION,
                        generation,
                        filter_sha256: validated.filter_sha256.clone(),
                        last_modified_at_unix_ms: entry.source_modified_at_unix_ms,
                        last_session_id: entry.session_id.clone().unwrap_or_default(),
                        last_session_ref: entry.session_ref.clone(),
                    })
                })
                .transpose()?
        } else {
            None
        };
        let metadata = metadata.unwrap_or(SessionCatalogWorkspaceMetadata {
            generation: 0,
            reconciled_at_unix_ms: 0,
            degraded_source_count: 0,
            identity_conflict_count: 0,
            truncated_source_count: 0,
        });
        drop(statement);
        transaction.commit()?;
        Ok(SessionCatalogProjectionPage {
            workspace_id: self.lifecycle.workspace_id.clone(),
            generation,
            reconciled_at_unix_ms: metadata.reconciled_at_unix_ms,
            degraded_source_count: metadata.degraded_source_count,
            identity_conflict_count: metadata.identity_conflict_count,
            truncated_source_count: metadata.truncated_source_count,
            entries,
            next_cursor,
        })
    }
}

fn validate_query(
    workspace_id: &str,
    query: SessionCatalogProjectionQuery,
) -> Result<ValidatedSessionCatalogQuery, SessionCatalogProjectionError> {
    if query.limit == 0 || query.limit > MAX_SESSION_CATALOG_PAGE_SIZE {
        return Err(SessionCatalogProjectionError::InvalidQuery {
            message: format!("limit must be between 1 and {MAX_SESSION_CATALOG_PAGE_SIZE}"),
        });
    }
    let search = normalize_optional_text(query.search, SESSION_CATALOG_SEARCH_MAX_BYTES, "search")?
        .map(|value| value.to_lowercase());
    let provider_name = normalize_optional_text(
        query.provider_name,
        SESSION_CATALOG_PROVIDER_MAX_BYTES,
        "provider_name",
    )?;
    let normalized = NormalizedSessionCatalogFilter {
        schema_version: SESSION_CATALOG_QUERY_SCHEMA_VERSION,
        workspace_id,
        search: search.as_deref(),
        provider_name: provider_name.as_deref(),
        pinned: query.pinned,
        source_state: query.source_state.map(catalog_state_name),
        sort: "modified_at_unix_ms_desc,session_id_desc,session_ref_desc",
    };
    let filter_sha256 = digest_filter(&normalized)?;
    let cursor = query
        .cursor
        .map(|cursor| decode_cursor(&cursor))
        .transpose()?;
    if let Some(cursor) = &cursor
        && cursor.filter_sha256 != filter_sha256
    {
        return Err(SessionCatalogProjectionError::InvalidCursor {
            message: "cursor does not belong to the requested filters".to_owned(),
        });
    }
    let search_pattern = search
        .as_deref()
        .map(escape_like_pattern)
        .map(|search| format!("%{search}%"));
    Ok(ValidatedSessionCatalogQuery {
        limit: query.limit,
        cursor,
        search_pattern,
        provider_name,
        pinned: query.pinned,
        source_state: query.source_state,
        filter_sha256,
    })
}

fn normalize_optional_text(
    value: Option<String>,
    max_bytes: usize,
    field: &'static str,
) -> Result<Option<String>, SessionCatalogProjectionError> {
    value
        .map(|value| {
            let value = value.trim().to_owned();
            if value.is_empty() {
                return Err(SessionCatalogProjectionError::InvalidQuery {
                    message: format!("{field} must not be blank"),
                });
            }
            if value.len() > max_bytes {
                return Err(SessionCatalogProjectionError::InvalidQuery {
                    message: format!("{field} exceeds {max_bytes} bytes"),
                });
            }
            if value.chars().any(char::is_control) {
                return Err(SessionCatalogProjectionError::InvalidQuery {
                    message: format!("{field} contains control characters"),
                });
            }
            Ok(value)
        })
        .transpose()
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn digest_filter<T: Serialize>(value: &T) -> Result<String, SessionCatalogProjectionError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| SessionCatalogProjectionError::Encoding {
            message: error.to_string(),
        })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn encode_cursor(
    cursor: &SessionCatalogPageCursorV1,
) -> Result<String, SessionCatalogProjectionError> {
    let bytes =
        serde_json::to_vec(cursor).map_err(|error| SessionCatalogProjectionError::Encoding {
            message: error.to_string(),
        })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(
    encoded: &str,
) -> Result<SessionCatalogPageCursorV1, SessionCatalogProjectionError> {
    if encoded.is_empty() || encoded.len() > 2_048 {
        return Err(SessionCatalogProjectionError::InvalidCursor {
            message: "cursor length is invalid".to_owned(),
        });
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        SessionCatalogProjectionError::InvalidCursor {
            message: "cursor is not valid base64url".to_owned(),
        }
    })?;
    let cursor: SessionCatalogPageCursorV1 = serde_json::from_slice(&bytes).map_err(|_| {
        SessionCatalogProjectionError::InvalidCursor {
            message: "cursor payload is invalid".to_owned(),
        }
    })?;
    if cursor.schema_version != SESSION_CATALOG_QUERY_SCHEMA_VERSION
        || cursor.filter_sha256.len() != 64
        || cursor.last_session_id.len() > 256
        || cursor.last_session_ref.is_empty()
        || cursor.last_session_ref.len() > 512
    {
        return Err(SessionCatalogProjectionError::InvalidCursor {
            message: "cursor contract is invalid".to_owned(),
        });
    }
    Ok(cursor)
}
