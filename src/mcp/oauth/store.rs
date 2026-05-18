//! Storage seam for the OAuth tables. Two stores:
//!   - [`McpOAuthClientStore`] — per-(org, issuer) registered DCR clients,
//!     with encrypted `client_secret` + `registration_access_token`.
//!   - [`McpOAuthPendingStore`] — short-lived (state, server_id, …) rows
//!     bridging `POST /oauth/start` to `GET /oauth/callback`.
//!
//! Both surface only validated domain types; ciphertext + nonces stay
//! inside the impls.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::{OrgId, UserId};
use crate::mcp::McpServerId;
use crate::types::SecretString;

use super::errors::OAuthError;

crate::str_enum! {
    /// Token-endpoint authentication method as defined by RFC 7591 §2.
    /// The DB CHECK constraint and the wire format both key off these
    /// labels; adding a method is a one-line edit.
    pub enum TokenAuthMethod {
        None              => "none",
        ClientSecretBasic => "client_secret_basic",
        ClientSecretPost  => "client_secret_post",
    }
}

/// New OAuth-client record from a DCR response, ready to persist.
/// Holds plaintext secrets briefly — the store seals them before INSERT.
#[derive(Debug, Clone)]
pub struct NewOAuthClient {
    pub org_id: OrgId,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<SecretString>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_client_uri: Option<String>,
    pub registration_access_token: Option<SecretString>,
    pub token_endpoint_auth_method: TokenAuthMethod,
    pub scope: Option<String>,
}

/// Decrypted DCR client returned by a lookup.
#[derive(Debug, Clone)]
pub struct DcrClientRecord {
    pub org_id: OrgId,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<SecretString>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub token_endpoint_auth_method: TokenAuthMethod,
    pub scope: Option<String>,
}

#[async_trait]
pub trait McpOAuthClientStore: fmt::Debug + Send + Sync {
    /// Insert-or-return: if `(org_id, issuer)` already has a row, return
    /// the existing one verbatim — DCR responses are idempotent for our
    /// purposes (the same org+vendor always uses the same registered
    /// client_id). Caller decides whether to use it or perform a fresh
    /// DCR; in v1 we always reuse.
    async fn upsert(&self, new: NewOAuthClient) -> Result<DcrClientRecord, OAuthError>;

    async fn read(
        &self,
        org_id: OrgId,
        issuer: &str,
    ) -> Result<Option<DcrClientRecord>, OAuthError>;
}

pub type SharedMcpOAuthClientStore = Arc<dyn McpOAuthClientStore>;

#[derive(Debug, Clone)]
pub struct PendingAuthorizationWrite {
    pub state: String,
    pub server_id: McpServerId,
    pub user_id: UserId,
    pub org_id: OrgId,
    pub issuer: String,
    pub pkce_verifier: String,
    pub redirect_to: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingAuthorization {
    pub state: String,
    pub server_id: McpServerId,
    pub user_id: UserId,
    pub org_id: OrgId,
    pub issuer: String,
    pub pkce_verifier: String,
    pub redirect_to: Option<String>,
}

#[async_trait]
pub trait McpOAuthPendingStore: fmt::Debug + Send + Sync {
    async fn insert(&self, row: PendingAuthorizationWrite) -> Result<(), OAuthError>;

    /// One-shot: read + delete in a single statement so the row cannot
    /// be replayed even if the AS issues two callbacks for the same
    /// state. Returns `None` for unknown / expired states.
    async fn consume(
        &self,
        state: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<PendingAuthorization>, OAuthError>;
}

pub type SharedMcpOAuthPendingStore = Arc<dyn McpOAuthPendingStore>;
