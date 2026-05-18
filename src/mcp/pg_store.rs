//! Postgres-backed [`McpServerStore`].

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::clock::SharedClock;

use super::error::McpError;
use super::limits::MAX_MCP_SERVERS;
use super::store::{McpHealthUpdate, McpServerCreate, McpServerStore, McpServerUpdate};
use super::types::{
    DiscoveredTool, McpDescription, McpServerAlias, McpServerId, McpServerRecord, McpTransport,
};

/// Transaction-scoped advisory-lock key used by `create` to serialise the count + insert
/// pair against the `MAX_MCP_SERVERS` cap. Released automatically on commit/rollback.
/// The literal is `0x6D63705F637265` (= ASCII "mcp_cre") — chosen for readability and
/// for not colliding with any other advisory lock the system uses.
const MCP_CREATE_LOCK_KEY: i64 = 0x006D_6370_5F63_7265;

/// Column list for the `McpServerRow` `FromRow` impl. Centralised so the
/// list stays in lockstep with the struct shape across every query in
/// this module. Macro form (vs `const &str`) so call sites can splice the
/// projection into a `concat!`-built compile-time literal and avoid
/// `format!`-assembled SQL (CLAUDE.md §10).
macro_rules! mcp_row_cols {
    () => {
        "id, org_id, alias, enabled, config, description, last_seen_at, last_error, \
         discovered_tools, created_by_user_id, connection_status, created_at, updated_at"
    };
}

pub struct PgMcpServerStore {
    pool: PgPool,
    clock: SharedClock,
    server_cap: usize,
}

impl PgMcpServerStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self::with_caps(pool, clock, MAX_MCP_SERVERS)
    }

    #[must_use]
    pub fn with_caps(pool: PgPool, clock: SharedClock, server_cap: usize) -> Self {
        Self {
            pool,
            clock,
            server_cap,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now_utc()
    }
}

impl fmt::Debug for PgMcpServerStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgMcpServerStore")
            .field("server_cap", &self.server_cap)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl McpServerStore for PgMcpServerStore {
    async fn create(&self, payload: McpServerCreate) -> Result<McpServerRecord, McpError> {
        let McpServerCreate {
            org_id,
            created_by_user_id,
            alias,
            config,
            description,
            enabled,
            connection_status,
        } = payload;
        let now = self.now();
        let id = McpServerId::new();
        let config_json = serde_json::to_value(&config)
            .map_err(|e| McpError::Backend(format!("serialize transport: {e}")))?;
        let server_cap = self.server_cap;
        let alias_str = alias.clone();

        // Store-internal SQL runs privileged because the registry refresher
        // and the HTTP handler share this trait; the per-org cap check
        // below needs to count across the org (the RLS policy already
        // restricts to the caller's orgs when an HTTP handler calls in
        // through a tenant tx, but we serve both paths uniformly here).
        // The tenant boundary is provided by the explicit `org_id` column.
        crate::auth::run_privileged::<(), McpError>(&self.pool, async |tx| {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(MCP_CREATE_LOCK_KEY)
                .execute(&mut **tx)
                .await?;
            let count: (i64,) =
                sqlx::query_as("SELECT COUNT(*)::BIGINT FROM mcp_servers WHERE org_id = $1")
                    .bind(org_id)
                    .fetch_one(&mut **tx)
                    .await?;
            let server_cap_i64 = i64::try_from(server_cap)
                .expect("invariant: MAX_MCP_SERVERS is a small constant that fits in i64");
            if count.0 >= server_cap_i64 {
                return Err(McpError::ServerCapExceeded { max: server_cap });
            }
            sqlx::query(
                "INSERT INTO mcp_servers \
                 (id, org_id, alias, enabled, config, description, \
                  created_by_user_id, connection_status, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
            )
            .bind(id)
            .bind(org_id)
            .bind(alias_str.as_str())
            .bind(enabled)
            .bind(&config_json)
            .bind(description.as_ref().map(McpDescription::as_str))
            .bind(created_by_user_id)
            .bind(connection_status)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(|e| map_unique_violation(e, &alias_str))?;
            Ok(())
        })
        .await?;

        Ok(McpServerRecord {
            id,
            org_id,
            alias,
            enabled,
            config,
            description,
            last_seen_at: None,
            last_error: None,
            discovered_tools: None,
            created_by_user_id,
            connection_status,
            created_at: now,
            updated_at: now,
        })
    }

    async fn list(&self) -> Result<Vec<McpServerRecord>, McpError> {
        let rows =
            crate::auth::run_privileged::<Vec<McpServerRow>, McpError>(&self.pool, async |tx| {
                Ok(sqlx::query_as::<_, McpServerRow>(concat!(
                    "SELECT ",
                    mcp_row_cols!(),
                    " FROM mcp_servers ORDER BY alias ASC",
                ))
                .fetch_all(&mut **tx)
                .await?)
            })
            .await?;
        rows.into_iter().map(McpServerRow::into_record).collect()
    }

    async fn list_enabled(&self) -> Result<Vec<McpServerRecord>, McpError> {
        // `auth_pending` rows are intentionally skipped: the registry
        // refresher would otherwise connect with no Authorization header
        // and the upstream's "Auth required" response would land as a
        // misleading `last_error` while the operator is still mid-flow
        // in the browser. `mark_connected` flips the row to 'ok' after
        // the OAuth callback persists credentials.
        let rows =
            crate::auth::run_privileged::<Vec<McpServerRow>, McpError>(&self.pool, async |tx| {
                Ok(sqlx::query_as::<_, McpServerRow>(concat!(
                    "SELECT ",
                    mcp_row_cols!(),
                    " FROM mcp_servers \
                     WHERE enabled = TRUE AND connection_status <> 'auth_pending' \
                     ORDER BY alias ASC",
                ))
                .fetch_all(&mut **tx)
                .await?)
            })
            .await?;
        rows.into_iter().map(McpServerRow::into_record).collect()
    }

    async fn read(
        &self,
        id: McpServerId,
        org_id: crate::auth::OrgId,
    ) -> Result<McpServerRecord, McpError> {
        // Privileged tx (RLS is bypassed) but the `WHERE … AND org_id = $2`
        // makes the cross-tenant fence explicit at the query layer — even
        // a future HTTP handler that forgets the tenant pre-gate cannot
        // fetch another org's row by id.
        let row =
            crate::auth::run_privileged::<Option<McpServerRow>, McpError>(&self.pool, async |tx| {
                Ok(sqlx::query_as::<_, McpServerRow>(concat!(
                    "SELECT ",
                    mcp_row_cols!(),
                    " FROM mcp_servers WHERE id = $1 AND org_id = $2",
                ))
                .bind(id)
                .bind(org_id)
                .fetch_optional(&mut **tx)
                .await?)
            })
            .await?;
        row.map_or_else(|| Err(McpError::NotFound(id)), McpServerRow::into_record)
    }

    async fn update(
        &self,
        id: McpServerId,
        org_id: crate::auth::OrgId,
        payload: McpServerUpdate,
    ) -> Result<McpServerRecord, McpError> {
        let now = self.now();
        crate::auth::run_privileged::<McpServerRecord, McpError>(&self.pool, async |tx| {
            let existing: Option<McpServerRow> = sqlx::query_as::<_, McpServerRow>(concat!(
                "SELECT ",
                mcp_row_cols!(),
                " FROM mcp_servers WHERE id = $1 AND org_id = $2 FOR UPDATE",
            ))
            .bind(id)
            .bind(org_id)
            .fetch_optional(&mut **tx)
            .await?;
            let existing = existing.ok_or(McpError::NotFound(id))?;
            let mut current = existing.into_record()?;

            if let Some(alias) = payload.alias {
                current.alias = alias;
            }
            if let Some(config) = payload.config {
                current.config = config;
            }
            if let Some(description) = payload.description {
                current.description = description;
            }
            if let Some(enabled) = payload.enabled {
                current.enabled = enabled;
            }
            current.updated_at = now;

            let config_json = serde_json::to_value(&current.config)
                .map_err(|e| McpError::Backend(format!("serialize transport: {e}")))?;

            sqlx::query(
                "UPDATE mcp_servers SET alias = $3, enabled = $4, config = $5, description = $6, \
                                        updated_at = $7 \
                 WHERE id = $1 AND org_id = $2",
            )
            .bind(id)
            .bind(org_id)
            .bind(current.alias.as_str())
            .bind(current.enabled)
            .bind(&config_json)
            .bind(current.description.as_ref().map(McpDescription::as_str))
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(|e| map_unique_violation(e, &current.alias))?;
            Ok(current)
        })
        .await
    }

    async fn delete(&self, id: McpServerId, org_id: crate::auth::OrgId) -> Result<(), McpError> {
        let rows_affected = crate::auth::run_privileged::<u64, McpError>(&self.pool, async |tx| {
            let res = sqlx::query("DELETE FROM mcp_servers WHERE id = $1 AND org_id = $2")
                .bind(id)
                .bind(org_id)
                .execute(&mut **tx)
                .await?;
            Ok(res.rows_affected())
        })
        .await?;
        if rows_affected == 0 {
            return Err(McpError::NotFound(id));
        }
        Ok(())
    }

    async fn update_health(
        &self,
        id: McpServerId,
        org_id: crate::auth::OrgId,
        health: McpHealthUpdate,
    ) -> Result<(), McpError> {
        let discovered_json = health
            .discovered_tools
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| McpError::Backend(format!("serialize discovered_tools: {e}")))?;
        let now = self.now();
        let rows_affected = crate::auth::run_privileged::<u64, McpError>(&self.pool, async |tx| {
            let res = sqlx::query(
                "UPDATE mcp_servers SET last_seen_at = $3, last_error = $4, \
                                        discovered_tools = COALESCE($5, discovered_tools), \
                                        updated_at = $6 \
                 WHERE id = $1 AND org_id = $2",
            )
            .bind(id)
            .bind(org_id)
            .bind(health.last_seen_at)
            .bind(health.last_error.as_deref())
            .bind(discovered_json)
            .bind(now)
            .execute(&mut **tx)
            .await?;
            Ok(res.rows_affected())
        })
        .await?;
        if rows_affected == 0 {
            return Err(McpError::NotFound(id));
        }
        Ok(())
    }
}

fn map_unique_violation(err: sqlx::Error, alias: &McpServerAlias) -> McpError {
    if let sqlx::Error::Database(db) = &err {
        // 23505 == unique_violation in Postgres SQLSTATE.
        if db.code().as_deref() == Some("23505") {
            return McpError::AliasTaken(alias.as_str().to_owned());
        }
    }
    McpError::Db(err)
}

#[derive(sqlx::FromRow)]
struct McpServerRow {
    id: McpServerId,
    org_id: crate::auth::OrgId,
    alias: String,
    enabled: bool,
    config: serde_json::Value,
    description: Option<String>,
    last_seen_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    discovered_tools: Option<serde_json::Value>,
    created_by_user_id: crate::auth::UserId,
    connection_status: super::types::ConnectionStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl McpServerRow {
    fn into_record(self) -> Result<McpServerRecord, McpError> {
        let alias = McpServerAlias::try_from(self.alias).map_err(McpError::Parse)?;
        let config: McpTransport = serde_json::from_value(self.config)
            .map_err(|e| McpError::Backend(format!("deserialize transport: {e}")))?;
        let description = self
            .description
            .map(McpDescription::try_from)
            .transpose()
            .map_err(McpError::Parse)?;
        let discovered_tools = self
            .discovered_tools
            .map(serde_json::from_value::<Vec<DiscoveredTool>>)
            .transpose()
            .map_err(|e| McpError::Backend(format!("deserialize discovered: {e}")))?;
        Ok(McpServerRecord {
            id: self.id,
            org_id: self.org_id,
            alias,
            enabled: self.enabled,
            config,
            description,
            last_seen_at: self.last_seen_at,
            last_error: self.last_error,
            discovered_tools,
            created_by_user_id: self.created_by_user_id,
            connection_status: self.connection_status,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
