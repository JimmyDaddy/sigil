use std::str;

use thiserror::Error as ThisError;

/// Bearer-token validator for the HTTP adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAuthValidator {
    expected_token: Option<String>,
}

impl HttpAuthValidator {
    /// Creates a validator that accepts requests without an Authorization header.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            expected_token: None,
        }
    }

    /// Creates a validator that requires `Bearer <token>`.
    #[must_use]
    pub(crate) fn required(token: impl Into<String>) -> Self {
        Self {
            expected_token: Some(token.into()),
        }
    }

    /// Returns whether requests must present a bearer token.
    #[must_use]
    pub fn token_required(&self) -> bool {
        self.expected_token.is_some()
    }

    /// Validates one raw Authorization header value.
    ///
    /// # Errors
    ///
    /// Returns an error when auth is required and the header is missing, malformed, or invalid.
    pub fn validate_authorization_header(
        &self,
        authorization: Option<&str>,
    ) -> Result<(), HttpAuthError> {
        let Some(expected_token) = self.expected_token.as_deref() else {
            return Ok(());
        };
        let Some(header) = authorization
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(HttpAuthError::MissingAuthorization);
        };
        let Some((scheme, token)) = header.split_once(' ') else {
            return Err(HttpAuthError::InvalidAuthorizationScheme);
        };
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return Err(HttpAuthError::InvalidAuthorizationScheme);
        }
        if token.trim() != expected_token {
            return Err(HttpAuthError::InvalidToken);
        }
        Ok(())
    }
}

/// Authentication errors returned by the HTTP adapter boundary.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpAuthError {
    /// Token auth is enabled but the configured token source did not produce a token.
    #[error("http auth token is missing from {token_env}")]
    MissingToken { token_env: String },
    /// The request did not include an Authorization header.
    #[error("http authorization header is required")]
    MissingAuthorization,
    /// The Authorization header did not use the Bearer scheme.
    #[error("http authorization header must use bearer token auth")]
    InvalidAuthorizationScheme,
    /// The bearer token did not match the configured token.
    #[error("http bearer token is invalid")]
    InvalidToken,
}
