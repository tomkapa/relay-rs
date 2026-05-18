//! Postgres-backed impls of the OAuth stores.

use std::fmt;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::auth::{OrgId, UserId};
use crate::clock::SharedClock;
use crate::crypto::{EncryptedBlob, SharedOrgEncryptor};
use crate::mcp::McpServerId;
use crate::types::SecretString;

use super::errors::OAuthError;
use super::store::{
    DcrClientRecord, McpOAuthClientStore, McpOAuthPendingStore, NewOAuthClient,
    PendingAuthorization, PendingAuthorizationWrite,
};

pub struct PgMcpOAuthClientStore {
    pool: PgPool,
    clock: SharedClock,
    enc: SharedOrgEncryptor,
}

impl PgMcpOAuthClientStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock, enc: SharedOrgEncryptor) -> Self {
        Self { pool, clock, enc }
    }
}

impl fmt::Debug for PgMcpOAuthClientStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgMcpOAuthClientStore")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl McpOAuthClientStore for PgMcpOAuthClientStore {
    async fn upsert(&self, new: NewOAuthClient) -> Result<DcrClientRecord, OAuthError> {
        // Seal secret + registration access token under the org KEK.
        let (secret_cipher, secret_nonce) = match new.client_secret.as_ref() {
            Some(s) => {
                let blob = self.enc.seal(new.org_id, s.expose().as_bytes())?;
                (Some(blob.ciphertext), Some(blob.nonce.to_vec()))
            }
            None => (None, None),
        };
        let (rat_cipher, rat_nonce) = match new.registration_access_token.as_ref() {
            Some(s) => {
                let blob = self.enc.seal(new.org_id, s.expose().as_bytes())?;
                (Some(blob.ciphertext), Some(blob.nonce.to_vec()))
            }
            None => (None, None),
        };
        let now = self.clock.now_utc();
        let key_version = crate::crypto::CURRENT_KEY_VERSION;

        // Try insert; if (org_id, issuer) already exists, return the
        // existing row. This is the "idempotent DCR" stance the trait
        // describes — re-registering for the same vendor is a no-op.
        crate::auth::run_privileged::<(), OAuthError>(&self.pool, async |tx| {
            sqlx::query(
                "INSERT INTO mcp_oauth_clients \
                 (org_id, issuer, client_id, authorization_endpoint, token_endpoint, \
                  registration_client_uri, registration_access_token_ciphertext, \
                  registration_access_token_nonce, client_secret_ciphertext, \
                  client_secret_nonce, key_version, token_endpoint_auth_method, scope, \
                  created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
                 ON CONFLICT (org_id, issuer) DO NOTHING",
            )
            .bind(new.org_id)
            .bind(&new.issuer)
            .bind(&new.client_id)
            .bind(&new.authorization_endpoint)
            .bind(&new.token_endpoint)
            .bind(new.registration_client_uri.as_deref())
            .bind(rat_cipher.as_deref())
            .bind(rat_nonce.as_deref())
            .bind(secret_cipher.as_deref())
            .bind(secret_nonce.as_deref())
            .bind(key_version)
            .bind(&new.token_endpoint_auth_method)
            .bind(new.scope.as_deref())
            .bind(now)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
        .await?;
        // Always re-read so we surface the *winning* row when the insert
        // raced with another caller.
        self.read(new.org_id, &new.issuer)
            .await?
            .ok_or_else(|| OAuthError::Misconfigured("oauth client vanished after upsert".into()))
    }

    async fn read(
        &self,
        org_id: OrgId,
        issuer: &str,
    ) -> Result<Option<DcrClientRecord>, OAuthError> {
        let row = crate::auth::run_privileged::<Option<OAuthClientRow>, OAuthError>(
            &self.pool,
            async |tx| {
                Ok(sqlx::query_as::<_, OAuthClientRow>(
                    "SELECT org_id, issuer, client_id, authorization_endpoint, token_endpoint, \
                            client_secret_ciphertext, client_secret_nonce, key_version, \
                            token_endpoint_auth_method, scope \
                     FROM mcp_oauth_clients WHERE org_id = $1 AND issuer = $2",
                )
                .bind(org_id)
                .bind(issuer)
                .fetch_optional(&mut **tx)
                .await?)
            },
        )
        .await?;
        let Some(row) = row else { return Ok(None) };
        let client_secret = row.decode_client_secret(&self.enc)?;
        Ok(Some(DcrClientRecord {
            org_id: row.org_id,
            issuer: row.issuer,
            client_id: row.client_id,
            client_secret,
            authorization_endpoint: row.authorization_endpoint,
            token_endpoint: row.token_endpoint,
            token_endpoint_auth_method: row.token_endpoint_auth_method,
            scope: row.scope,
        }))
    }
}

#[derive(sqlx::FromRow)]
struct OAuthClientRow {
    org_id: OrgId,
    issuer: String,
    client_id: String,
    authorization_endpoint: String,
    token_endpoint: String,
    client_secret_ciphertext: Option<Vec<u8>>,
    client_secret_nonce: Option<Vec<u8>>,
    key_version: i16,
    token_endpoint_auth_method: String,
    scope: Option<String>,
}

impl OAuthClientRow {
    fn decode_client_secret(
        &self,
        enc: &SharedOrgEncryptor,
    ) -> Result<Option<SecretString>, OAuthError> {
        let (Some(c), Some(n)) = (
            self.client_secret_ciphertext.as_ref(),
            self.client_secret_nonce.as_ref(),
        ) else {
            return Ok(None);
        };
        let nonce: [u8; crate::crypto::NONCE_BYTES] = n.as_slice().try_into().map_err(|_| {
            OAuthError::Misconfigured("oauth client_secret nonce wrong length".into())
        })?;
        let blob = EncryptedBlob {
            key_version: self.key_version,
            nonce,
            ciphertext: c.clone(),
        };
        let plaintext = enc.open(self.org_id, &blob)?;
        let s = std::str::from_utf8(plaintext.as_slice())
            .map_err(|_| OAuthError::Misconfigured("oauth client_secret not utf-8".into()))?;
        Ok(Some(SecretString::try_from(s.to_owned()).map_err(|e| {
            OAuthError::Misconfigured(format!("oauth client_secret invalid: {e}"))
        })?))
    }
}

pub struct PgMcpOAuthPendingStore {
    pool: PgPool,
    clock: SharedClock,
}

impl PgMcpOAuthPendingStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self { pool, clock }
    }
}

impl fmt::Debug for PgMcpOAuthPendingStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgMcpOAuthPendingStore")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl McpOAuthPendingStore for PgMcpOAuthPendingStore {
    async fn insert(&self, row: PendingAuthorizationWrite) -> Result<(), OAuthError> {
        let now = self.clock.now_utc();
        crate::auth::run_privileged::<(), OAuthError>(&self.pool, async |tx| {
            sqlx::query(
                "INSERT INTO mcp_oauth_pending \
                 (state, server_id, user_id, org_id, issuer, pkce_verifier, redirect_to, \
                  created_at, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(&row.state)
            .bind(row.server_id)
            .bind(row.user_id)
            .bind(row.org_id)
            .bind(&row.issuer)
            .bind(&row.pkce_verifier)
            .bind(row.redirect_to.as_deref())
            .bind(now)
            .bind(row.expires_at)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
        .await
    }

    async fn consume(
        &self,
        state: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<PendingAuthorization>, OAuthError> {
        crate::auth::run_privileged::<Option<PendingAuthorization>, OAuthError>(
            &self.pool,
            async |tx| {
                let row = sqlx::query_as::<_, PendingRow>(
                    "DELETE FROM mcp_oauth_pending \
                     WHERE state = $1 AND expires_at > $2 \
                     RETURNING state, server_id, user_id, org_id, issuer, pkce_verifier, \
                               redirect_to",
                )
                .bind(state)
                .bind(now)
                .fetch_optional(&mut **tx)
                .await?;
                Ok(row.map(PendingRow::into_record))
            },
        )
        .await
    }
}

#[derive(sqlx::FromRow)]
struct PendingRow {
    state: String,
    server_id: McpServerId,
    user_id: UserId,
    org_id: OrgId,
    issuer: String,
    pkce_verifier: String,
    redirect_to: Option<String>,
}

impl PendingRow {
    fn into_record(self) -> PendingAuthorization {
        PendingAuthorization {
            state: self.state,
            server_id: self.server_id,
            user_id: self.user_id,
            org_id: self.org_id,
            issuer: self.issuer,
            pkce_verifier: self.pkce_verifier,
            redirect_to: self.redirect_to,
        }
    }
}
