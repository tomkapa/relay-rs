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
use axum::routing::{delete, get, post, put};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{AuthError, Principal};
use crate::mcp::{
    DiscoveredTool, McpClient, McpDescription, McpError, McpServerAlias, McpServerCreate,
    McpServerId, McpServerRecord, McpServerUpdate, McpTransport,
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
}

/// What we hand back on every CRUD response. Mirrors `mcp_servers` plus a flag
/// telling the operator whether the row is currently exposed by the live registry.
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
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<McpServerRecord> for McpServerResponse {
    fn from(r: McpServerRecord) -> Self {
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
    state.mcp_refresh.request();
    Ok((StatusCode::CREATED, Json(record.into())))
}

async fn list_mcp_servers(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<McpServerResponse>>, HttpError> {
    // Tenant-scoped read: open a tx, set `app.user_id` via the GUC, and
    // let the `mcp_servers_org_isolation` RLS policy do the filtering.
    // Bypasses the store's privileged read path so the user can see only
    // their own org's rows.
    let mut tx = crate::auth::begin_as(&state.pool, &principal).await?;
    let rows = sqlx::query_as::<_, McpServerRowForList>(
        "SELECT id, org_id, alias, enabled, config, description, last_seen_at, last_error, \
                discovered_tools, created_by_user_id, created_at, updated_at \
         FROM mcp_servers ORDER BY alias ASC",
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
        "SELECT id, org_id, alias, enabled, config, description, last_seen_at, last_error, \
                discovered_tools, created_by_user_id, created_at, updated_at \
         FROM mcp_servers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    let row = row.ok_or(HttpError::NotFound)?;
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
    state.mcp_refresh.request();
    Ok(Json(row.into()))
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

/// Request body for `POST /mcp-servers/test-connect`. Carries a full transport
/// config exactly as a create request would, *except* that nothing is persisted
/// — the handler builds an [`McpClient`] in-process, performs the MCP
/// `initialize` handshake plus one `list_tools` round-trip, then drops the
/// client. The 401-redact contract still applies: the request body may carry
/// secret bearer tokens in `config.headers`, so on every response we surface
/// either the tool list (on success) or a free-text error string — never echo
/// the input back.
#[derive(Debug, Deserialize)]
struct TestConnectRequest {
    config: McpTransport,
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
    let connect = McpClient::connect(&payload.config).await;
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
    let alias_prefix = "test"; // No alias on a test-connect; we still need a
    // stable prefix for any rendered tool name. Frontends ignore the prefix —
    // the names are only here so the UI can render "found N tools" alongside
    // their remote names. Using a fixed prefix avoids leaking real aliases.

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
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
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
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
