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
    ClientProvenance, DcrClientRecord, McpOAuthClientStore, McpOAuthPendingStore, NewOAuthClient,
    PendingAuthorization, PendingAuthorizationWrite, TokenAuthMethod,
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

/// `ON CONFLICT` fragment selected by [`ClientProvenance`]. DCR rows are
/// insert-or-return (no-op assignment forces RETURNING for the existing
/// row); operator rows fully overwrite and force `registration_*` NULL.
/// Kept as a `&'static str` so the SQL string is composed once at
/// monomorphization time, not per call.
const ON_CONFLICT_DCR_KEEP: &str =
    "ON CONFLICT (org_id, issuer) DO UPDATE SET issuer = mcp_oauth_clients.issuer";
const ON_CONFLICT_OPERATOR_OVERWRITE: &str = "ON CONFLICT (org_id, issuer) DO UPDATE SET \
        client_id = EXCLUDED.client_id, \
        authorization_endpoint = EXCLUDED.authorization_endpoint, \
        token_endpoint = EXCLUDED.token_endpoint, \
        registration_client_uri = NULL, \
        registration_access_token_ciphertext = NULL, \
        registration_access_token_nonce = NULL, \
        client_secret_ciphertext = EXCLUDED.client_secret_ciphertext, \
        client_secret_nonce = EXCLUDED.client_secret_nonce, \
        key_version = EXCLUDED.key_version, \
        token_endpoint_auth_method = EXCLUDED.token_endpoint_auth_method, \
        scope = EXCLUDED.scope";

#[async_trait]
impl McpOAuthClientStore for PgMcpOAuthClientStore {
    async fn upsert(&self, new: NewOAuthClient) -> Result<DcrClientRecord, OAuthError> {
        let (rcu, rat, on_conflict) = match &new.provenance {
            ClientProvenance::Dcr {
                registration_client_uri,
                registration_access_token,
            } => (
                registration_client_uri.as_deref(),
                registration_access_token.as_ref(),
                ON_CONFLICT_DCR_KEEP,
            ),
            ClientProvenance::Operator => (None, None, ON_CONFLICT_OPERATOR_OVERWRITE),
        };
        let (secret_cipher, secret_nonce) =
            seal_optional(&self.enc, new.org_id, new.client_secret.as_ref())?;
        let (rat_cipher, rat_nonce) = seal_optional(&self.enc, new.org_id, rat)?;
        let now = self.clock.now_utc();
        let key_version = crate::crypto::CURRENT_KEY_VERSION;
        let sql = format!(
            "INSERT INTO mcp_oauth_clients \
             (org_id, issuer, client_id, authorization_endpoint, token_endpoint, \
              registration_client_uri, registration_access_token_ciphertext, \
              registration_access_token_nonce, client_secret_ciphertext, \
              client_secret_nonce, key_version, token_endpoint_auth_method, scope, \
              created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
             {on_conflict} \
             RETURNING org_id, issuer, client_id, authorization_endpoint, \
                       token_endpoint, client_secret_ciphertext, client_secret_nonce, \
                       key_version, token_endpoint_auth_method, scope",
        );
        let row =
            crate::auth::run_privileged::<OAuthClientRow, OAuthError>(&self.pool, async |tx| {
                Ok(sqlx::query_as::<_, OAuthClientRow>(&sql)
                    .bind(new.org_id)
                    .bind(&new.issuer)
                    .bind(&new.client_id)
                    .bind(&new.authorization_endpoint)
                    .bind(&new.token_endpoint)
                    .bind(rcu)
                    .bind(rat_cipher.as_deref())
                    .bind(rat_nonce.as_deref())
                    .bind(secret_cipher.as_deref())
                    .bind(secret_nonce.as_deref())
                    .bind(key_version)
                    .bind(new.token_endpoint_auth_method)
                    .bind(new.scope.as_deref())
                    .bind(now)
                    .fetch_one(&mut **tx)
                    .await?)
            })
            .await?;
        row.into_record(&self.enc)
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
        row.map(|r| r.into_record(&self.enc)).transpose()
    }
}

/// Pair of (ciphertext, nonce) bytes for an optional `SecretString`
/// column. Tuple is concrete so the call site stays readable; the
/// inner `Option`s pair the column nullability invariant (both NULL or
/// both set), enforced by the schema's CHECK clauses.
type SealedColumn = (Option<Vec<u8>>, Option<Vec<u8>>);

fn seal_optional(
    enc: &SharedOrgEncryptor,
    org: OrgId,
    plaintext: Option<&SecretString>,
) -> Result<SealedColumn, OAuthError> {
    let Some(s) = plaintext else {
        return Ok((None, None));
    };
    let blob = enc.seal(org, s.expose().as_bytes())?;
    Ok((Some(blob.ciphertext), Some(blob.nonce.to_vec())))
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
    token_endpoint_auth_method: TokenAuthMethod,
    scope: Option<String>,
}

impl OAuthClientRow {
    fn into_record(self, enc: &SharedOrgEncryptor) -> Result<DcrClientRecord, OAuthError> {
        let client_secret = self.decode_client_secret(enc)?;
        Ok(DcrClientRecord {
            org_id: self.org_id,
            issuer: self.issuer,
            client_id: self.client_id,
            client_secret,
            authorization_endpoint: self.authorization_endpoint,
            token_endpoint: self.token_endpoint,
            token_endpoint_auth_method: self.token_endpoint_auth_method,
            scope: self.scope,
        })
    }

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
