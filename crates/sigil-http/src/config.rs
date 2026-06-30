use std::{
    error::Error,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use serde::{Deserialize, Serialize};

use crate::auth::{HttpAuthError, HttpAuthValidator};

/// Environment variable read by the HTTP adapter for its bearer token by default.
pub const DEFAULT_HTTP_TOKEN_ENV: &str = "SIGIL_HTTP_TOKEN";

/// Configuration for the local HTTP/SSE adapter.
///
/// This crate is intentionally transport-thin: it owns HTTP-facing DTOs and will
/// delegate agent execution to `sigil-runtime` and shared contracts from `sigil-kernel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpServerConfig {
    /// Interface address the server should bind to.
    pub bind_host: IpAddr,
    /// TCP port to bind. `0` lets the operating system choose an available local port.
    pub port: u16,
    /// Authentication controls for HTTP clients.
    pub auth: HttpAuthConfig,
}

impl HttpServerConfig {
    /// Returns the configured bind address.
    #[must_use]
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_host, self.port)
    }

    /// Returns whether the adapter is configured to accept only loopback traffic.
    #[must_use]
    pub fn is_loopback_only(&self) -> bool {
        self.bind_host.is_loopback()
    }

    /// Returns whether bearer-token authentication is required.
    #[must_use]
    pub fn token_required(&self) -> bool {
        self.auth.require_token
    }

    /// Validates safety invariants that are independent from any concrete HTTP framework.
    ///
    /// # Errors
    ///
    /// Returns an error when token auth is required but has no environment variable,
    /// or when a non-loopback bind disables token auth.
    pub fn validate(&self) -> Result<(), HttpServerConfigError> {
        if self.auth.require_token && self.auth.token_env.trim().is_empty() {
            return Err(HttpServerConfigError::MissingTokenEnv);
        }
        if !self.is_loopback_only() && !self.auth.require_token {
            return Err(HttpServerConfigError::ExternalBindWithoutToken);
        }
        Ok(())
    }
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            auth: HttpAuthConfig::default(),
        }
    }
}

/// Authentication controls for the HTTP/SSE adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct HttpAuthConfig {
    /// Require clients to send a bearer token.
    pub require_token: bool,
    /// Environment variable containing the bearer token.
    pub token_env: String,
}

impl Default for HttpAuthConfig {
    fn default() -> Self {
        Self {
            require_token: true,
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        }
    }
}

impl HttpAuthConfig {
    /// Builds a bearer-token validator from an already resolved token value.
    ///
    /// # Errors
    ///
    /// Returns an error when token auth is required but no non-empty token was provided.
    pub fn validator_from_token(
        &self,
        token: Option<&str>,
    ) -> Result<HttpAuthValidator, HttpAuthError> {
        if !self.require_token {
            return Ok(HttpAuthValidator::disabled());
        }
        let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) else {
            return Err(HttpAuthError::MissingToken {
                token_env: self.token_env.clone(),
            });
        };
        Ok(HttpAuthValidator::required(token))
    }
}

/// Configuration validation errors for the HTTP/SSE adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpServerConfigError {
    /// Token auth is enabled but no environment variable name was configured.
    MissingTokenEnv,
    /// A non-loopback bind address cannot disable token auth.
    ExternalBindWithoutToken,
}

impl fmt::Display for HttpServerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTokenEnv => {
                write!(
                    f,
                    "http auth token env must be set when token auth is required"
                )
            }
            Self::ExternalBindWithoutToken => {
                write!(
                    f,
                    "http token auth is required for non-loopback bind addresses"
                )
            }
        }
    }
}

impl Error for HttpServerConfigError {}
