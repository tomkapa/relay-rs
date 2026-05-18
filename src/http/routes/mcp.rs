//! CRUD endpoints for the MCP server registry.
//!
//! `POST /mcp-servers`               — create
//! `GET  /mcp-servers`               — list
//! `GET  /mcp-servers/{id}`          — read one
//! `PUT  /mcp-servers/{id}`          — update
//! `DELETE /mcp-servers/{id}`        — delete
//! `POST /mcp-servers/test-connect`  — validate a candidate config without persisting
//!
//! Every mutating handler signals the long-running MCP refresh coordinator (via the
//! cheap clone-able [`McpRefreshTrigger`]) so the registered tools become callable on
//! the next prompt without a restart. The CRUD response itself does not wait on the
//! refresh — operator visibility comes from the `last_seen_at`/`last_error`/
//! `discovered_tools` columns surfaced on the next read.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use axum::routing::{delete, get, post, put};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{AuthError, Principal};
use crate::mcp::oauth::{
    NewOAuthClient, OAuthError, PendingAuthorization, build_authorize_url,
    discover_authorization_server, exchange_code, register_dynamic_client,
};
use crate::mcp::{
    CredentialPayload, DiscoveredTool, MCP_CREDENTIAL_READ_TIMEOUT, McpClient, McpCredentialWrite,
    McpDescription, McpError, McpServerAlias, McpServerCreate, McpServerId, McpServerRecord,
    McpServerUpdate, McpTransport, OAUTH2_KIND_LABEL, OAuth2Payload,
};

use super::super::error::HttpError;
use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        // The static path goes first so `/mcp-servers/test-connect` is not
        // captured by the `{id}` route.
        .route("/mcp-servers/test-connect", post(test_connect_mcp_server))
        .route(
            "/mcp-servers",
            post(create_mcp_server).get(list_mcp_servers),
        )
        .route(
            "/mcp-servers/{id}",
            get(read_mcp_server)
                .merge(put(update_mcp_server))
                .merge(delete(delete_mcp_server)),
        )
        .route(
            "/mcp-servers/{id}/credentials",
            put(put_mcp_credentials).merge(delete(delete_mcp_credentials)),
        )
        .route("/mcp-servers/{id}/oauth/start", post(start_oauth))
        .route("/mcp-servers/{id}/oauth/disconnect", post(disconnect_oauth))
}

/// Public router (no auth middleware) for the OAuth callback. The browser
/// is returning from the vendor's consent screen with `?state=&code=`;
/// CSRF protection comes from the PKCE `state` column being a one-shot
/// row, not the session cookie. Merged into the public subtree alongside
/// `auth::router()`.
pub(super) fn oauth_callback_router() -> Router<AppState> {
    Router::new().route("/mcp-oauth/callback", get(handle_oauth_callback))
}

/// What we hand back on every CRUD response. Mirrors `mcp_servers` plus a flag
/// telling the operator whether the row is currently exposed by the live registry.
///
/// **Never carries credential plaintext.** Secrets live in the encrypted
/// `mcp_server_credentials` table; the wire shape surfaces only "are
/// credentials set?" + "what kind?", so the UI can render a state badge
/// without the backend ever echoing the secret value back to the caller.
#[derive(Debug, Serialize)]
struct McpServerResponse {
    id: McpServerId,
    alias: String,
    enabled: bool,
    config: McpTransport,
    description: Option<String>,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
    discovered_tools: Option<Vec<DiscoveredTool>>,
    created_by_user_id: crate::auth::UserId,
    /// `false` when no row exists in `mcp_server_credentials` for this
    /// server; `true` otherwise. The frontend uses this to render a state
    /// badge without us ever decrypting the payload.
    has_credentials: bool,
    /// `None` when `has_credentials = false`; otherwise the stable
    /// `kind` label (`"static_headers"` or `"oauth2"`).
    credentials_kind: Option<String>,
    /// Surfaced connection state — defaults to `"ok"`; the OAuth refresher
    /// (phase D) flips it when a refresh token is revoked.
    connection_status: crate::mcp::ConnectionStatus,
    /// Email of the user who created this connection (joined from `users`).
    /// `None` only if the row was created before tenancy enforcement and the
    /// FK is null — never the case for newly-created rows.
    creator_email: Option<String>,
    /// `None` on the list / create / update paths; only the single-server read
    /// path decrypts the credential blob to surface this.
    token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl McpServerResponse {
    fn from_record(r: McpServerRecord, credentials_kind: Option<String>) -> Self {
        Self {
            id: r.id,
            alias: r.alias.as_str().to_owned(),
            enabled: r.enabled,
            config: r.config,
            description: r.description.map(|d| d.as_str().to_owned()),
            last_seen_at: r.last_seen_at,
            last_error: r.last_error,
            discovered_tools: r.discovered_tools,
            created_by_user_id: r.created_by_user_id,
            has_credentials: credentials_kind.is_some(),
            credentials_kind,
            connection_status: r.connection_status,
            creator_email: None,
            token_expires_at: None,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateMcpServerRequest {
    alias: String,
    config: McpTransport,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
    /// Optional credentials, set in the same request the row is created in.
    /// When present, sealed under the org KEK and written to
    /// `mcp_server_credentials` before the create returns.
    #[serde(default)]
    credentials: Option<CredentialInput>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct UpdateMcpServerRequest {
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    config: Option<McpTransport>,
    /// HTTP PATCH semantics: outer `Option` distinguishes "field omitted (no change)"
    /// from "field present (set or clear)"; inner `Option` carries the new value
    /// (`null` clears the column). Clippy's suggested "custom enum" alternative is
    /// strictly more boilerplate for the same shape.
    #[serde(default, deserialize_with = "double_option::deserialize")]
    #[allow(clippy::option_option)] // explicit two-state representation; see doc comment
    description: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};

    #[allow(clippy::option_option)] // see UpdateMcpServerRequest::description
    pub(super) fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Deserialize::deserialize(d).map(Some)
    }
}

async fn create_mcp_server(
    State(state): State<AppState>,
    principal: Principal,
    Json(payload): Json<CreateMcpServerRequest>,
) -> Result<(StatusCode, Json<McpServerResponse>), HttpError> {
    let alias = McpServerAlias::try_from(payload.alias).map_err(HttpError::Parse)?;
    let description = payload
        .description
        .map(McpDescription::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let credentials_payload = match payload.credentials {
        Some(c) => {
            c.check_caps()?;
            Some(c.into_payload())
        }
        None => None,
    };

    let record = state
        .mcp_store
        .create(McpServerCreate {
            org_id: principal.active_org_id,
            created_by_user_id: principal.user_id,
            alias,
            config: payload.config,
            description,
            enabled: payload.enabled,
        })
        .await?;

    let credentials_kind = if let Some(payload) = credentials_payload {
        let kind = payload.kind_label().to_owned();
        state
            .mcp_credentials
            .upsert(McpCredentialWrite {
                server_id: record.id,
                org_id: principal.active_org_id,
                payload,
            })
            .await?;
        Some(kind)
    } else {
        None
    };

    state.mcp_refresh.request();
    Ok((
        StatusCode::CREATED,
        Json(McpServerResponse::from_record(record, credentials_kind)),
    ))
}

async fn list_mcp_servers(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<McpServerResponse>>, HttpError> {
    // Tenant-scoped read: open a tx, set `app.user_id` via the GUC, and
    // let the `mcp_servers_org_isolation` RLS policy do the filtering.
    // Bypasses the store's privileged read path so the user can see only
    // their own org's rows. The LEFT JOIN onto `mcp_server_credentials`
    // surfaces only the `kind` label — never the ciphertext — so the
    // response can render `has_credentials` without an extra round-trip.
    let mut tx = crate::auth::begin_as(&state.pool, &principal).await?;
    let rows = sqlx::query_as::<_, McpServerRowForList>(
        "SELECT s.id, s.org_id, s.alias, s.enabled, s.config, s.description, \
                s.last_seen_at, s.last_error, s.discovered_tools, \
                s.created_by_user_id, s.connection_status, s.created_at, s.updated_at, \
                c.kind AS credentials_kind, u.email AS creator_email \
         FROM mcp_servers s \
         LEFT JOIN mcp_server_credentials c ON c.server_id = s.id \
         LEFT JOIN users u ON u.id = s.created_by_user_id \
         ORDER BY s.alias ASC",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(r.try_into_response()?);
    }
    Ok(Json(out))
}

async fn read_mcp_server(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerResponse>, HttpError> {
    let id = McpServerId::from(id);
    let mut tx = crate::auth::begin_as(&state.pool, &principal).await?;
    let row = sqlx::query_as::<_, McpServerRowForList>(
        "SELECT s.id, s.org_id, s.alias, s.enabled, s.config, s.description, \
                s.last_seen_at, s.last_error, s.discovered_tools, \
                s.created_by_user_id, s.connection_status, s.created_at, s.updated_at, \
                c.kind AS credentials_kind, u.email AS creator_email \
         FROM mcp_servers s \
         LEFT JOIN mcp_server_credentials c ON c.server_id = s.id \
         LEFT JOIN users u ON u.id = s.created_by_user_id \
         WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    let mut row = row.ok_or(HttpError::NotFound)?;
    if row.credentials_kind.as_deref() == Some(OAUTH2_KIND_LABEL) {
        let cred = tokio::time::timeout(
            MCP_CREDENTIAL_READ_TIMEOUT,
            state.mcp_credentials.read(id, principal.active_org_id),
        )
        .await
        .map_err(|_| {
            tracing::warn!(event = "mcp.read.credential_read_timeout");
            HttpError::Internal
        })??;
        if let Some(record) = cred
            && let CredentialPayload::Oauth2(oauth) = &record.payload
        {
            row.token_expires_at = Some(oauth.expires_at);
        }
    }
    Ok(Json(row.try_into_response()?))
}

async fn update_mcp_server(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateMcpServerRequest>,
) -> Result<Json<McpServerResponse>, HttpError> {
    let id = McpServerId::from(id);
    let alias = payload
        .alias
        .map(McpServerAlias::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let description = payload
        .description
        .map(|inner| inner.map(McpDescription::try_from).transpose())
        .transpose()
        .map_err(HttpError::Parse)?;
    // Tenant gate: 404 cross-org / unknown ids without leaking existence
    // before dispatching the privileged update.
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::McpServers,
        id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    let row = state
        .mcp_store
        .update(
            id,
            principal.active_org_id,
            McpServerUpdate {
                alias,
                config: payload.config,
                description,
                enabled: payload.enabled,
            },
        )
        .await?;
    // Look up the credential kind label after the update so the response
    // surface remains uniform across CRUD endpoints.
    let credentials_kind = state
        .mcp_credentials
        .read(row.id, principal.active_org_id)
        .await?
        .map(|c| c.payload.kind_label().to_owned());
    state.mcp_refresh.request();
    Ok(Json(McpServerResponse::from_record(row, credentials_kind)))
}

async fn delete_mcp_server(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let id = McpServerId::from(id);
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::McpServers,
        id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    state.mcp_store.delete(id, principal.active_org_id).await?;
    state.mcp_refresh.request();
    Ok(StatusCode::NO_CONTENT)
}

/// PUT `/mcp-servers/{id}/credentials` — replace (or insert) the credential
/// row for `id`. Always writes fresh ciphertext without reading the old
/// one back, so a replacement cannot expose the prior credential.
async fn put_mcp_credentials(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(payload): Json<CredentialInput>,
) -> Result<StatusCode, HttpError> {
    payload.check_caps()?;
    let server_id = McpServerId::from(id);
    // Tenant gate: 404 cross-org / unknown ids without leaking existence.
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::McpServers,
        server_id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    state
        .mcp_credentials
        .upsert(McpCredentialWrite {
            server_id,
            org_id: principal.active_org_id,
            payload: payload.into_payload(),
        })
        .await?;
    state.mcp_refresh.request();
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE `/mcp-servers/{id}/credentials` — drop the credential row.
/// Idempotent: no body, returns 204 whether or not a row existed.
async fn delete_mcp_credentials(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let server_id = McpServerId::from(id);
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::McpServers,
        server_id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    state
        .mcp_credentials
        .delete(server_id, principal.active_org_id)
        .await?;
    state.mcp_refresh.request();
    Ok(StatusCode::NO_CONTENT)
}

/// Boundary shape for credential input on create / replace / test paths.
///
/// Matches the on-disk [`CredentialPayload`] enum: the wire form carries a
/// `kind` discriminant and the variant payload. The OAuth flow does not
/// populate credentials through this path (the callback writes them
/// directly); only `static_headers` is accepted here today.
///
/// Validation: every header name and value passes through its own newtype
/// smart constructor; the map size is bounded by the
/// [`crate::mcp::MCP_MAX_HEADERS`] cap, checked in the route handler.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CredentialInput {
    StaticHeaders {
        headers: std::collections::BTreeMap<crate::mcp::McpHeaderName, crate::mcp::McpHeaderValue>,
    },
}

impl CredentialInput {
    fn into_payload(self) -> CredentialPayload {
        match self {
            Self::StaticHeaders { headers } => CredentialPayload::StaticHeaders { headers },
        }
    }

    /// Validate boundary-only caps (the per-newtype parsers already cover
    /// length/charset; this catches the map-size limit that no parser sees).
    fn check_caps(&self) -> Result<(), HttpError> {
        match self {
            Self::StaticHeaders { headers } => {
                if headers.len() > crate::mcp::MCP_MAX_HEADERS {
                    return Err(HttpError::BadRequest(format!(
                        "credentials: too many headers (max {})",
                        crate::mcp::MCP_MAX_HEADERS
                    )));
                }
                Ok(())
            }
        }
    }
}

/// Request body for `POST /mcp-servers/test-connect`. Carries a transport
/// config plus optional credentials; nothing is persisted — the handler
/// builds an [`McpClient`] in-process, performs the MCP `initialize`
/// handshake plus one `list_tools` round-trip, then drops the client. The
/// secret-redact contract still applies: the request body may carry bearer
/// tokens in `credentials.headers`, so on every response we surface either
/// the tool list (on success) or a free-text error string — never echo the
/// input back.
#[derive(Debug, Deserialize)]
struct TestConnectRequest {
    config: McpTransport,
    #[serde(default)]
    credentials: Option<CredentialInput>,
}

/// Response shape: a single discriminant indicates success vs. failure so the
/// frontend can render a clear pass/fail state without parsing error strings.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum TestConnectResponse {
    Ok {
        discovered_tools: Vec<DiscoveredTool>,
    },
    Failed {
        error: String,
    },
}

async fn test_connect_mcp_server(
    State(state): State<AppState>,
    principal: Principal,
    Json(payload): Json<TestConnectRequest>,
) -> Result<Json<TestConnectResponse>, HttpError> {
    // Per-user rate limit, enforced before we open any outbound connection.
    // This is the SSRF guardrail: a logged-in user can probe at most
    // `MCP_TEST_CONNECT_PER_MIN` distinct URLs per rolling minute.
    if !state.mcp_test_rate.try_admit(principal.user_id) {
        return Err(HttpError::TooManyRequests);
    }

    let span = tracing::info_span!(
        "mcp.test_connect",
        relay.user.id = %principal.user_id,
        relay.org.id = %principal.active_org_id,
    );
    let _guard = span.enter();

    // Connect + list_tools inside the operator-trusted MCP-client path. Both
    // calls are bounded by their own internal timeouts (MCP_CONNECT_TIMEOUT,
    // MCP_LIST_TOOLS_TIMEOUT). Failures collapse to a structured 200 response
    // body — the call itself succeeded from a transport perspective even
    // when the upstream MCP server refused the handshake.
    let credentials = payload.credentials.map(CredentialInput::into_payload);
    let connect = McpClient::connect(&payload.config, credentials.as_ref()).await;
    let client = match connect {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(error = %e, "mcp.test_connect.connect_failed");
            return Ok(Json(TestConnectResponse::Failed {
                error: redact_error(&e),
            }));
        }
    };

    let McpTransport::Http { url, .. } = &payload.config;
    // Fixed prefix on test-connect: we have no alias yet, and using one of
    // the user's real aliases would leak it through the rendered name.
    let alias_prefix = "test";

    let listed = match client.list_tools().await {
        Ok(t) => t,
        Err(e) => {
            tracing::info!(error = %e, "mcp.test_connect.list_failed");
            return Ok(Json(TestConnectResponse::Failed {
                error: redact_error(&e),
            }));
        }
    };

    let discovered: Vec<DiscoveredTool> = listed
        .into_iter()
        .map(|t| {
            let remote_name = t.name.to_string();
            DiscoveredTool {
                prefixed_name: format!("mcp_{alias_prefix}_{remote_name}"),
                description: t.description.as_deref().map(str::to_owned),
                remote_name,
            }
        })
        .collect();

    tracing::info!(
        relay.mcp.url = %url.as_str(),
        relay.mcp.discovered = discovered.len(),
        "mcp.test_connect.ok"
    );
    Ok(Json(TestConnectResponse::Ok {
        discovered_tools: discovered,
    }))
}

/// Strip any potentially-sensitive sub-strings (currently a no-op: McpError's
/// Display impls already omit bearer-token-carrying header bytes) and clamp
/// the message length so a stack-traced underlying error can't bloat the
/// response. Kept as a single seam so a future format-string regression can
/// be patched in one place.
fn redact_error(err: &McpError) -> String {
    const MAX: usize = 512;
    let s = err.to_string();
    if s.len() > MAX {
        // Floor by char boundary, not byte: an MCP error string is ASCII in
        // practice, but a stray UTF-8 sequence inside Url::parse output is
        // possible. `s.is_char_boundary` walks at most 3 bytes back.
        let mut cut = MAX;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &s[..cut])
    } else {
        s
    }
}

// Local row type for the tenant-scoped SELECTs. Mirrors the columns
// returned by the store's read path but lives here so the route can
// run raw SQL inside the principal-scoped tx without going through
// the store's privileged transaction.
#[derive(sqlx::FromRow)]
struct McpServerRowForList {
    id: McpServerId,
    org_id: crate::auth::OrgId,
    alias: String,
    enabled: bool,
    config: serde_json::Value,
    description: Option<String>,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
    discovered_tools: Option<serde_json::Value>,
    created_by_user_id: crate::auth::UserId,
    connection_status: crate::mcp::ConnectionStatus,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    credentials_kind: Option<String>,
    creator_email: Option<String>,
    #[sqlx(default)]
    token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl McpServerRowForList {
    fn try_into_response(self) -> Result<McpServerResponse, HttpError> {
        // Rebuild the typed record exactly as the store does so the
        // response shape matches whether the call path is via the store
        // or via a tenant-scoped raw query here.
        let alias = McpServerAlias::try_from(self.alias).map_err(HttpError::Parse)?;
        let config: McpTransport = serde_json::from_value(self.config).map_err(|e| {
            tracing::error!(error = ?e, "mcp.row.deserialize_transport");
            HttpError::Internal
        })?;
        let description = self
            .description
            .map(McpDescription::try_from)
            .transpose()
            .map_err(HttpError::Parse)?;
        let discovered_tools = self
            .discovered_tools
            .map(serde_json::from_value::<Vec<DiscoveredTool>>)
            .transpose()
            .map_err(|e| {
                tracing::error!(error = ?e, "mcp.row.deserialize_discovered");
                HttpError::Internal
            })?;
        let _ = self.org_id; // not on the wire shape — RLS already filtered.
        Ok(McpServerResponse {
            id: self.id,
            alias: alias.as_str().to_owned(),
            enabled: self.enabled,
            config,
            description: description.map(|d| d.as_str().to_owned()),
            last_seen_at: self.last_seen_at,
            last_error: self.last_error,
            discovered_tools,
            created_by_user_id: self.created_by_user_id,
            has_credentials: self.credentials_kind.is_some(),
            credentials_kind: self.credentials_kind,
            connection_status: self.connection_status,
            creator_email: self.creator_email,
            token_expires_at: self.token_expires_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// ────────────────────────────────────────────────────────────────────────
// Upstream OAuth
// ────────────────────────────────────────────────────────────────────────

const OAUTH_CALLBACK_PATH: &str = "/mcp-oauth/callback";
/// How long a pending OAuth row stays valid. Long enough for the user to
/// complete the consent flow even with a slow network; short enough that
/// an abandoned row is reaped on schedule. Matches the spec's typical
/// "10 minutes" cap.
const OAUTH_PENDING_TTL: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Deserialize)]
struct OAuthStartRequest {
    /// Optional path the frontend wants us to redirect back to after the
    /// callback completes. Length-capped by the DB CHECK constraint;
    /// path-only by convention.
    #[serde(default)]
    redirect_to: Option<String>,
    /// Optional scope override. When absent we use whatever the AS
    /// advertises as default; vendor-specific scopes can be requested by
    /// the frontend (e.g. Notion needs `read_content read_user`).
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct OAuthStartResponse {
    authorize_url: String,
}

/// `POST /mcp-servers/{id}/oauth/start` — kick off the browser flow.
///
/// Steps:
///   1. Resolve server, ensure caller is authorized for it.
///   2. Discover authorization-server metadata (RFC 9728 + RFC 8414).
///   3. Look up the registered DCR client for `(org, issuer)`, or
///      register one via RFC 7591 if first time.
///   4. Mint PKCE + state, persist the pending row.
///   5. Build the authorize URL and hand it back.
#[allow(clippy::too_many_lines)] // composition path — branching is what each step is
async fn start_oauth(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<OAuthStartRequest>,
) -> Result<Json<OAuthStartResponse>, HttpError> {
    let server_id = McpServerId::from(id);

    // Step 1: tenant gate — look up the server and ensure the caller can
    // see it. The store read goes through `run_privileged` but the
    // explicit org_id filter pins the row to the principal's org.
    let server = state
        .mcp_store
        .read(server_id, principal.active_org_id)
        .await
        .map_err(HttpError::Mcp)?;
    let McpTransport::Http { url } = &server.config;

    // Step 2: discovery. Bounded by internal timeouts in
    // `discover_authorization_server`.
    let as_metadata = discover_authorization_server(&state.mcp_oauth_flow.http, url.as_str())
        .await
        .map_err(map_oauth_err)?;

    // Step 3: load-or-register DCR client.
    let redirect_uri = format!("{}{}", state.oauth_redirect_base, OAUTH_CALLBACK_PATH);
    let dcr = if let Some(existing) = state
        .mcp_oauth_clients
        .read(principal.active_org_id, &as_metadata.issuer)
        .await
        .map_err(map_oauth_err)?
    {
        existing
    } else {
        let new: NewOAuthClient = register_dynamic_client(
            &state.mcp_oauth_flow,
            &as_metadata,
            principal.active_org_id,
            &redirect_uri,
            body.scope.as_deref(),
        )
        .await
        .map_err(map_oauth_err)?;
        state
            .mcp_oauth_clients
            .upsert(new)
            .await
            .map_err(map_oauth_err)?
    };

    // Step 4: PKCE + state, persist pending row.
    let start =
        build_authorize_url(&dcr, &redirect_uri, body.scope.as_deref()).map_err(map_oauth_err)?;
    let now = state.clock.now_utc();
    let expires_at = now
        + chrono::Duration::from_std(OAUTH_PENDING_TTL)
            .expect("invariant: OAUTH_PENDING_TTL fits in chrono::Duration");
    state
        .mcp_oauth_pending
        .insert(crate::mcp::oauth::PendingAuthorizationWrite {
            state: start.state.clone(),
            server_id,
            user_id: principal.user_id,
            org_id: principal.active_org_id,
            issuer: dcr.issuer.clone(),
            pkce_verifier: start.pkce_verifier.clone(),
            redirect_to: body.redirect_to.clone(),
            expires_at,
        })
        .await
        .map_err(map_oauth_err)?;

    Ok(Json(OAuthStartResponse {
        authorize_url: start.authorize_url.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// `GET /mcp-oauth/callback?code=&state=`. Runs without a session
/// cookie — CSRF protection comes from `state` being a one-shot row.
/// Always returns a 303 redirect to the FE callback page with
/// `?status=ok` or `?status=failed&reason=…` so the user lands on a
/// rendered page either way.
#[tracing::instrument(name = "mcp.oauth.callback", skip_all)]
async fn handle_oauth_callback(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<OAuthCallbackQuery>,
) -> axum::response::Response {
    match callback_flow(&state, q).await {
        Ok(redirect_to) => {
            tracing::info!(event = "mcp.oauth.callback.ok");
            redirect_ok(redirect_to.as_deref())
        }
        Err(CallbackFail {
            redirect_to,
            reason,
        }) => {
            tracing::info!(event = "mcp.oauth.callback.failed", reason = %reason);
            redirect_failed(redirect_to.as_deref(), reason)
        }
    }
}

struct CallbackFail {
    redirect_to: Option<String>,
    reason: &'static str,
}

async fn callback_flow(
    state: &AppState,
    q: OAuthCallbackQuery,
) -> Result<Option<String>, CallbackFail> {
    if let Some(err) = q.error.as_deref() {
        tracing::warn!(
            event = "mcp.oauth.callback.vendor_error",
            error = %err,
            description = q.error_description.as_deref().unwrap_or(""),
        );
        let redirect_to = consume_pending_for_redirect(state, q.state.as_deref()).await;
        // The vendor's `error` token already passes through `sanitize_reason`
        // in `failed_redirect`, so a raw value here is safe.
        return Err(CallbackFail {
            redirect_to,
            reason: vendor_error_to_reason(err),
        });
    }
    let state_val = q.state.as_deref().ok_or(CallbackFail {
        redirect_to: None,
        reason: "state_missing",
    })?;
    let Some(code) = q.code.as_deref() else {
        let redirect_to = consume_pending_for_redirect(state, Some(state_val)).await;
        return Err(CallbackFail {
            redirect_to,
            reason: "code_missing",
        });
    };
    let now = state.clock.now_utc();
    let pending = consume_pending(state, state_val, now).await?;
    let dcr = load_dcr(state, &pending).await?;
    let token = exchange_token(state, &dcr, &pending, code, now).await?;
    persist_oauth_success(state, &pending, token, now)
        .await
        .map_err(|reason| CallbackFail {
            redirect_to: pending.redirect_to.clone(),
            reason,
        })?;
    Ok(pending.redirect_to)
}

fn redirect_ok(redirect_to: Option<&str>) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    Redirect::to(&ok_redirect(redirect_to)).into_response()
}

fn redirect_failed(redirect_to: Option<&str>, reason: &str) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    Redirect::to(&failed_redirect(redirect_to, reason)).into_response()
}

/// Convert a vendor `error` query token into our short reason key. Anything
/// other than the standard set falls through to a generic `vendor_error`
/// so we never leak unbounded vendor text into the FE URL.
fn vendor_error_to_reason(raw: &str) -> &'static str {
    match raw {
        "access_denied" => "access_denied",
        "invalid_scope" => "invalid_scope",
        "server_error" => "vendor_server_error",
        "temporarily_unavailable" => "vendor_unavailable",
        _ => "vendor_error",
    }
}

async fn consume_pending(
    state: &AppState,
    state_val: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PendingAuthorization, CallbackFail> {
    match state.mcp_oauth_pending.consume(state_val, now).await {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(CallbackFail {
            redirect_to: None,
            reason: "unknown_or_expired_state",
        }),
        Err(e) => {
            tracing::error!(
                event = "mcp.oauth.callback.consume_failed",
                error = ?e,
            );
            Err(CallbackFail {
                redirect_to: None,
                reason: "internal_error",
            })
        }
    }
}

async fn load_dcr(
    state: &AppState,
    pending: &PendingAuthorization,
) -> Result<crate::mcp::oauth::DcrClientRecord, CallbackFail> {
    match state
        .mcp_oauth_clients
        .read(pending.org_id, &pending.issuer)
        .await
    {
        Ok(Some(dcr)) => Ok(dcr),
        Ok(None) => Err(CallbackFail {
            redirect_to: pending.redirect_to.clone(),
            reason: "oauth_client_missing",
        }),
        Err(e) => {
            tracing::error!(
                event = "mcp.oauth.callback.client_lookup_failed",
                error = ?e,
            );
            Err(CallbackFail {
                redirect_to: pending.redirect_to.clone(),
                reason: "internal_error",
            })
        }
    }
}

async fn exchange_token(
    state: &AppState,
    dcr: &crate::mcp::oauth::DcrClientRecord,
    pending: &PendingAuthorization,
    code: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::mcp::oauth::TokenExchangeResult, CallbackFail> {
    let redirect_uri = format!("{}{}", state.oauth_redirect_base, OAUTH_CALLBACK_PATH);
    exchange_code(
        &state.mcp_oauth_flow,
        dcr,
        &redirect_uri,
        code,
        &pending.pkce_verifier,
        now,
    )
    .await
    .map_err(|e| {
        // Upstream/vendor failure — not our error, so `warn`. CLAUDE.md
        // §2: "ERROR = user-visible failure"; vendor refusing the
        // exchange is an operating error from our side.
        tracing::warn!(
            event = "mcp.oauth.callback.exchange_failed",
            error = ?e,
        );
        CallbackFail {
            redirect_to: pending.redirect_to.clone(),
            reason: "token_exchange_failed",
        }
    })
}

async fn persist_oauth_success(
    state: &AppState,
    pending: &PendingAuthorization,
    token: crate::mcp::oauth::TokenExchangeResult,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    let payload = CredentialPayload::Oauth2(OAuth2Payload {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: token.expires_at,
        scope: token.scope,
        issuer: token.issuer,
        token_endpoint: token.token_endpoint,
    });
    if let Err(e) = state
        .mcp_credentials
        .upsert(McpCredentialWrite {
            server_id: pending.server_id,
            org_id: pending.org_id,
            payload,
        })
        .await
    {
        tracing::error!(
            event = "mcp.oauth.callback.credentials_write_failed",
            error = ?e,
        );
        return Err("credentials_write_failed");
    }
    mark_connected(state, pending.server_id, pending.org_id, now)
        .await
        .map_err(|e| {
            tracing::error!(
                event = "mcp.oauth.callback.status_update_failed",
                error = ?e,
            );
            "internal_error"
        })?;
    state.mcp_refresh.request();
    Ok(())
}

// Privileged write: the callback runs without a session cookie, so we
// can't go through `begin_as`. `org_id = $2` pins the row to the tenant
// the pending row was issued for.
async fn mark_connected(
    state: &AppState,
    server_id: McpServerId,
    org_id: crate::auth::OrgId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), crate::auth::AuthError> {
    crate::auth::run_privileged::<(), crate::auth::AuthError>(&state.pool, async |tx| {
        sqlx::query(
            "UPDATE mcp_servers SET connection_status = 'ok', last_error = NULL, \
                                    updated_at = $3 \
             WHERE id = $1 AND org_id = $2",
        )
        .bind(server_id)
        .bind(org_id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        Ok(())
    })
    .await
}

/// Best-effort consumption of the pending row when we already know the
/// flow has failed and just want to extract the `redirect_to` so the FE
/// lands on the right page. Swallows errors and absence — the failure
/// redirect falls back to `/` either way.
async fn consume_pending_for_redirect(state: &AppState, raw_state: Option<&str>) -> Option<String> {
    let raw = raw_state?;
    let now = state.clock.now_utc();
    match state.mcp_oauth_pending.consume(raw, now).await {
        Ok(Some(p)) => p.redirect_to,
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(
                event = "mcp.oauth.callback.consume_for_redirect_failed",
                error = ?e,
            );
            None
        }
    }
}

/// Build the successful-callback redirect URL. The base path is the
/// caller-supplied `redirect_to` if it survives `sanitize_return_to`
/// (relative path, ≤2048 bytes); otherwise `/`. A `status=ok` marker is
/// appended so the FE polling loop terminates immediately on first nav.
fn ok_redirect(redirect_to: Option<&str>) -> String {
    let base = sanitized_base(redirect_to);
    append_query(&base, "status=ok")
}

/// Build the failed-callback redirect URL. Same allow-list as
/// [`ok_redirect`]; appends `status=failed` plus a sanitized short
/// `reason` token so the FE Failed frame can branch on it.
fn failed_redirect(redirect_to: Option<&str>, reason: &str) -> String {
    let base = sanitized_base(redirect_to);
    let safe_reason = sanitize_reason(reason);
    append_query(
        &base,
        &format!("status=failed&reason={}", encode_query_value(&safe_reason)),
    )
}

fn sanitized_base(redirect_to: Option<&str>) -> String {
    redirect_to
        .and_then(super::auth::sanitize_return_to)
        .unwrap_or_else(|| "/".to_owned())
}

fn append_query(base: &str, kv: &str) -> String {
    if base.contains('?') {
        format!("{base}&{kv}")
    } else {
        format!("{base}?{kv}")
    }
}

/// Clamp the failure reason to an ASCII-safe short token so a vendor
/// can't smuggle markup or unbounded text into our FE via the query
/// string. We keep `[a-z0-9_-]`, lowercase the rest into `_`, and cap
/// the length.
fn sanitize_reason(raw: &str) -> String {
    const MAX: usize = 64;
    let mut out = String::with_capacity(raw.len().min(MAX));
    for ch in raw.chars().take(MAX) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_owned()
    } else {
        out
    }
}

/// Minimal percent-encoder for a query value. Our reasons are already
/// `[a-z0-9_-]` after `sanitize_reason`, so this is a belt-and-braces
/// pass that escapes any stragglers without pulling in a new dep.
fn encode_query_value(raw: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[derive(Debug, Serialize)]
struct OAuthDisconnectResponse {
    ok: bool,
}

async fn disconnect_oauth(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Json<OAuthDisconnectResponse>, HttpError> {
    let server_id = McpServerId::from(id);
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::McpServers,
        server_id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    state
        .mcp_credentials
        .delete(server_id, principal.active_org_id)
        .await?;
    state.mcp_refresh.request();
    Ok(Json(OAuthDisconnectResponse { ok: true }))
}

fn map_oauth_err(err: OAuthError) -> HttpError {
    match err {
        OAuthError::InvalidState | OAuthError::Expired => HttpError::BadRequest(err.to_string()),
        OAuthError::Discovery(_) | OAuthError::Dcr(_) | OAuthError::TokenEndpoint(_) => {
            tracing::warn!(error = %err, "mcp.oauth.upstream_error");
            HttpError::BadRequest(err.to_string())
        }
        OAuthError::RefreshRevoked => HttpError::Conflict(err.to_string()),
        OAuthError::Crypto(_) | OAuthError::Db(_) | OAuthError::Misconfigured(_) => {
            tracing::error!(error = %err, "mcp.oauth.internal_error");
            HttpError::Internal
        }
        OAuthError::Mcp(e) => HttpError::Mcp(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_redirect_uses_root_when_caller_omitted_path() {
        assert_eq!(ok_redirect(None), "/?status=ok");
    }

    #[test]
    fn ok_redirect_appends_status_to_safe_path() {
        assert_eq!(
            ok_redirect(Some("/connections/oauth-callback?server_id=abc")),
            "/connections/oauth-callback?server_id=abc&status=ok"
        );
    }

    // open-redirect attempts get clamped to `/` via sanitize_return_to.
    #[test]
    fn ok_redirect_rejects_absolute_url() {
        assert_eq!(
            ok_redirect(Some("https://attacker.example/steal")),
            "/?status=ok"
        );
    }

    #[test]
    fn ok_redirect_rejects_protocol_relative_url() {
        assert_eq!(ok_redirect(Some("//attacker.example")), "/?status=ok");
    }

    #[test]
    fn failed_redirect_lowercases_and_escapes_reason() {
        assert_eq!(
            failed_redirect(None, "Access Denied!"),
            "/?status=failed&reason=access_denied_"
        );
    }

    #[test]
    fn failed_redirect_falls_back_when_reason_is_empty() {
        assert_eq!(failed_redirect(None, ""), "/?status=failed&reason=unknown");
    }

    #[test]
    fn sanitize_reason_caps_length_and_normalizes() {
        let long = "x".repeat(200);
        let out = sanitize_reason(&long);
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn encode_query_value_passes_through_safe_chars() {
        assert_eq!(encode_query_value("abc_123-XYZ.~"), "abc_123-XYZ.~");
    }

    #[test]
    fn encode_query_value_percent_encodes_unsafe_chars() {
        assert_eq!(encode_query_value("a b/c"), "a%20b%2Fc");
    }
}
