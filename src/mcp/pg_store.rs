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
        DateTime::<Utc>::from(self.clock.now_wall())
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
            alias,
            config,
            description,
            enabled,
        } = payload;
        let now = self.now();

        let mut tx = self.pool.begin().await?;
        // §6: serialise the cap-check + insert window. Without this lock two concurrent
        // creates can both observe `count == cap-1` and both insert, breaching the cap.
        // The lock is transaction-scoped and released automatically on commit/rollback.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MCP_CREATE_LOCK_KEY)
            .execute(&mut *tx)
            .await?;
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*)::BIGINT FROM mcp_servers")
            .fetch_one(&mut *tx)
            .await?;
        let server_cap_i64 = i64::try_from(self.server_cap)
            .expect("invariant: MAX_MCP_SERVERS is a small constant that fits in i64");
        if count.0 >= server_cap_i64 {
            return Err(McpError::ServerCapExceeded {
                max: self.server_cap,
            });
        }

        let id = McpServerId::new();
        let config_json = serde_json::to_value(&config)
            .map_err(|e| McpError::Backend(format!("serialize transport: {e}")))?;

        let result = sqlx::query(
            "INSERT INTO mcp_servers \
             (id, alias, enabled, config, description, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(id)
        .bind(alias.as_str())
        .bind(enabled)
        .bind(&config_json)
        .bind(description.as_ref().map(McpDescription::as_str))
        .bind(now)
        .execute(&mut *tx)
        .await;

        if let Err(e) = result {
            return Err(map_unique_violation(e, &alias));
        }
        tx.commit().await?;

        Ok(McpServerRecord {
            id,
            alias,
            enabled,
            config,
            description,
            last_seen_at: None,
            last_error: None,
            discovered_tools: None,
            created_at: now,
            updated_at: now,
        })
    }

    async fn list(&self) -> Result<Vec<McpServerRecord>, McpError> {
        let rows = sqlx::query_as::<_, McpServerRow>(
            "SELECT id, alias, enabled, config, description, last_seen_at, last_error, \
                    discovered_tools, created_at, updated_at \
             FROM mcp_servers ORDER BY alias ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(McpServerRow::into_record).collect()
    }

    async fn list_enabled(&self) -> Result<Vec<McpServerRecord>, McpError> {
        let rows = sqlx::query_as::<_, McpServerRow>(
            "SELECT id, alias, enabled, config, description, last_seen_at, last_error, \
                    discovered_tools, created_at, updated_at \
             FROM mcp_servers WHERE enabled = TRUE ORDER BY alias ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(McpServerRow::into_record).collect()
    }

    async fn read(&self, id: McpServerId) -> Result<McpServerRecord, McpError> {
        let row = sqlx::query_as::<_, McpServerRow>(
            "SELECT id, alias, enabled, config, description, last_seen_at, last_error, \
                    discovered_tools, created_at, updated_at \
             FROM mcp_servers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map_or_else(|| Err(McpError::NotFound(id)), McpServerRow::into_record)
    }

    async fn update(
        &self,
        id: McpServerId,
        payload: McpServerUpdate,
    ) -> Result<McpServerRecord, McpError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        let existing: Option<McpServerRow> = sqlx::query_as::<_, McpServerRow>(
            "SELECT id, alias, enabled, config, description, last_seen_at, last_error, \
                    discovered_tools, created_at, updated_at \
             FROM mcp_servers WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
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

        let result = sqlx::query(
            "UPDATE mcp_servers SET alias = $2, enabled = $3, config = $4, description = $5, \
                                    updated_at = $6 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(current.alias.as_str())
        .bind(current.enabled)
        .bind(&config_json)
        .bind(current.description.as_ref().map(McpDescription::as_str))
        .bind(now)
        .execute(&mut *tx)
        .await;
        if let Err(e) = result {
            return Err(map_unique_violation(e, &current.alias));
        }
        tx.commit().await?;
        Ok(current)
    }

    async fn delete(&self, id: McpServerId) -> Result<(), McpError> {
        let res = sqlx::query("DELETE FROM mcp_servers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(McpError::NotFound(id));
        }
        Ok(())
    }

    async fn update_health(
        &self,
        id: McpServerId,
        health: McpHealthUpdate,
    ) -> Result<(), McpError> {
        let discovered_json = health
            .discovered_tools
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| McpError::Backend(format!("serialize discovered_tools: {e}")))?;
        let now = self.now();
        let res = sqlx::query(
            "UPDATE mcp_servers SET last_seen_at = $2, last_error = $3, \
                                    discovered_tools = COALESCE($4, discovered_tools), \
                                    updated_at = $5 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(health.last_seen_at)
        .bind(health.last_error.as_deref())
        .bind(discovered_json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
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
    alias: String,
    enabled: bool,
    config: serde_json::Value,
    description: Option<String>,
    last_seen_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    discovered_tools: Option<serde_json::Value>,
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
            alias,
            enabled: self.enabled,
            config,
            description,
            last_seen_at: self.last_seen_at,
            last_error: self.last_error,
            discovered_tools,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
