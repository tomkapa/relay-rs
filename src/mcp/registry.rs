//! Dynamic MCP tool registry.
//!
//! Holds the live MCP client connections and the tool specs derived from them. Refresh
//! is the only mutator; reads (`specs`, `get`) are lock-free in steady state because
//! the inner state is replaced as a whole `Arc`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use serde_json::Value;
use tracing::{instrument, warn};

use crate::clock::SharedClock;
use crate::provider::ToolSpec;
use crate::tools::{DynamicToolSource, SharedTool};
use crate::types::{ParseError, ToolName};

use super::client::McpClient;
use super::error::McpError;
use super::limits::{MAX_MCP_SERVERS, MAX_TOOLS_PER_SERVER};
use super::store::{McpHealthUpdate, SharedMcpServerStore};
use super::tool::McpTool;
use super::types::{DiscoveredTool, McpServerAlias, McpServerId, McpServerRecord, McpTransport};

/// Live tool catalogue derived from registered MCP servers.
///
/// The composition root holds one `Arc<McpRegistry>`; it is shared by the worker
/// (read path) and the HTTP CRUD handlers (which trigger `refresh` after every write).
pub struct McpRegistry {
    inner: RwLock<McpState>,
    store: SharedMcpServerStore,
    clock: SharedClock,
    server_cap: usize,
}

impl std::fmt::Debug for McpRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snapshot = self.inner.read().expect("registry lock poisoned");
        f.debug_struct("McpRegistry")
            .field("server_cap", &self.server_cap)
            .field("connected_servers", &snapshot.servers.len())
            .field("total_tools", &snapshot.by_name.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct McpState {
    by_name: HashMap<ToolName, SharedTool>,
    specs: Arc<[ToolSpec]>,
    /// Indexed by server id so we can reuse a client across refreshes when its config
    /// hasn't changed (avoids re-running the MCP `initialize` handshake on every CRUD
    /// edit to an unrelated row).
    servers: HashMap<McpServerId, ConnectedServer>,
}

struct ConnectedServer {
    config: McpTransport,
    client: Arc<McpClient>,
}

impl McpRegistry {
    /// Build an empty registry. Call [`refresh`](Self::refresh) once at startup before
    /// the worker pool begins claiming sessions, so the agent's first turn already sees
    /// the registered MCP tools.
    #[must_use]
    pub fn new(store: SharedMcpServerStore, clock: SharedClock) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(McpState::default()),
            store,
            clock,
            server_cap: MAX_MCP_SERVERS,
        })
    }

    /// Read-side handle: the (possibly-empty) flat slice of tool specs the agent
    /// concatenates with the static built-in registry every turn.
    #[must_use]
    pub fn specs(&self) -> Arc<[ToolSpec]> {
        self.inner
            .read()
            .expect("registry lock poisoned")
            .specs
            .clone()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<SharedTool> {
        self.inner
            .read()
            .expect("registry lock poisoned")
            .by_name
            .get(name)
            .cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("registry lock poisoned")
            .by_name
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Re-read the enabled servers from the store, reconnect any whose config changed,
    /// list their tools, and atomically replace the in-memory state.
    ///
    /// Per-server failures are isolated: a server that fails to connect or list tools
    /// is dropped from the live spec set with its `last_error` row updated, but
    /// healthy servers continue to serve their tools.
    #[instrument(name = "mcp.registry.refresh", skip_all, err)]
    pub async fn refresh(&self) -> Result<(), McpError> {
        let rows = self.store.list_enabled().await?;
        // §6 / §5: cap is enforced at create-time, but we belt-and-brace at refresh too
        // — a misconfigured deploy or a DB hand-edit shouldn't blow the budget here.
        if rows.len() > self.server_cap {
            warn!(
                relay.mcp.rows = rows.len(),
                relay.mcp.cap = self.server_cap,
                "mcp.refresh.cap_exceeded",
            );
        }
        let rows: Vec<McpServerRecord> = rows.into_iter().take(self.server_cap).collect();

        // Snapshot existing connections so we can reuse / drop them.
        let mut prior: HashMap<McpServerId, ConnectedServer> = {
            let mut guard = self.inner.write().expect("registry lock poisoned");
            std::mem::take(&mut guard.servers)
        };

        let mut servers: HashMap<McpServerId, ConnectedServer> = HashMap::with_capacity(rows.len());
        let mut by_name: HashMap<ToolName, SharedTool> = HashMap::new();
        let mut specs: Vec<ToolSpec> = Vec::new();

        for row in rows {
            self.refresh_one(row, &mut prior, &mut servers, &mut by_name, &mut specs)
                .await;
        }

        specs.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        let new_state = McpState {
            by_name,
            specs: Arc::from(specs),
            servers,
        };
        *self.inner.write().expect("registry lock poisoned") = new_state;
        // `prior` (any servers no longer present) is dropped here, which terminates
        // their rmcp worker tasks.
        drop(prior);
        Ok(())
    }

    /// Per-server work for [`refresh`]. Connect (or reuse), list tools, populate the
    /// new state buffers. Failures are logged and reflected in `last_error`; a single
    /// bad server never aborts the whole refresh.
    async fn refresh_one(
        &self,
        row: McpServerRecord,
        prior: &mut HashMap<McpServerId, ConnectedServer>,
        servers: &mut HashMap<McpServerId, ConnectedServer>,
        by_name: &mut HashMap<ToolName, SharedTool>,
        specs: &mut Vec<ToolSpec>,
    ) {
        let McpServerRecord {
            id, alias, config, ..
        } = row;

        let client = match self.connect_or_reuse(id, &alias, &config, prior).await {
            Some(c) => c,
            None => return,
        };

        let remote_tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    relay.mcp.server.id = %id,
                    relay.mcp.server.alias = %alias,
                    error = %e,
                    "mcp.refresh.list_tools_failed",
                );
                let _ = self
                    .store
                    .update_health(
                        id,
                        McpHealthUpdate {
                            last_seen_at: None,
                            last_error: Some(format!("list_tools: {e}")),
                            discovered_tools: None,
                        },
                    )
                    .await;
                return;
            }
        };

        let now = chrono::DateTime::<Utc>::from(self.clock.now_wall());
        let discovered = ingest_tools(&alias, &client, remote_tools, by_name, specs);

        let _ = self
            .store
            .update_health(
                id,
                McpHealthUpdate {
                    last_seen_at: Some(now),
                    last_error: None,
                    discovered_tools: Some(discovered),
                },
            )
            .await;

        servers.insert(id, ConnectedServer { config, client });
    }

    /// Reuse an existing client when its config hasn't changed; otherwise open a new
    /// one. `None` on connect failure (the row has had its `last_error` updated).
    async fn connect_or_reuse(
        &self,
        id: McpServerId,
        alias: &McpServerAlias,
        config: &McpTransport,
        prior: &mut HashMap<McpServerId, ConnectedServer>,
    ) -> Option<Arc<McpClient>> {
        if let Some(prev) = prior.remove(&id)
            && &prev.config == config
        {
            return Some(prev.client);
        }
        match McpClient::connect(config).await {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                warn!(
                    relay.mcp.server.id = %id,
                    relay.mcp.server.alias = %alias,
                    error = %e,
                    "mcp.refresh.connect_failed",
                );
                let _ = self
                    .store
                    .update_health(
                        id,
                        McpHealthUpdate {
                            last_seen_at: None,
                            last_error: Some(format!("connect: {e}")),
                            discovered_tools: None,
                        },
                    )
                    .await;
                None
            }
        }
    }
}

/// Build [`McpTool`] / [`ToolSpec`] entries for one server's remote tool list. Returns
/// the per-row `discovered_tools` snapshot that the store records.
fn ingest_tools(
    alias: &McpServerAlias,
    client: &Arc<McpClient>,
    remote_tools: Vec<rmcp::model::Tool>,
    by_name: &mut HashMap<ToolName, SharedTool>,
    specs: &mut Vec<ToolSpec>,
) -> Vec<DiscoveredTool> {
    let mut discovered: Vec<DiscoveredTool> = Vec::with_capacity(remote_tools.len());
    for remote in remote_tools.into_iter().take(MAX_TOOLS_PER_SERVER) {
        let prefixed = match prefixed_name(alias, remote.name.as_ref()) {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    relay.mcp.server.alias = %alias,
                    relay.mcp.tool.remote = %remote.name,
                    error = %e,
                    "mcp.tool.name_too_long",
                );
                continue;
            }
        };
        if by_name.contains_key(&prefixed) {
            // A different server already produced this prefixed name (alias collision
            // or duplicated remote name); skip the duplicate so registry building stays
            // total. Operators see the survivor in `discovered_tools`.
            warn!(
                relay.tool = %prefixed,
                "mcp.tool.duplicate_skipped",
            );
            continue;
        }
        let description: Arc<str> = Arc::from(
            remote
                .description
                .as_deref()
                .unwrap_or("(no description provided)"),
        );
        let schema = Arc::new(Value::Object((*remote.input_schema).clone()));
        discovered.push(DiscoveredTool {
            remote_name: remote.name.to_string(),
            prefixed_name: prefixed.as_str().to_owned(),
            description: remote.description.as_deref().map(str::to_owned),
        });
        let tool = Arc::new(McpTool::new(
            prefixed.clone(),
            remote.name.to_string(),
            description.clone(),
            schema.clone(),
            client.clone(),
        ));
        specs.push(ToolSpec {
            name: prefixed.clone(),
            description,
            input_schema: schema,
        });
        by_name.insert(prefixed, tool);
    }
    discovered
}

fn prefixed_name(alias: &McpServerAlias, remote: &str) -> Result<ToolName, ParseError> {
    let raw = format!("mcp_{}_{}", alias.as_str(), remote);
    ToolName::try_from(raw.as_str())
}

impl DynamicToolSource for McpRegistry {
    fn specs(&self) -> Arc<[ToolSpec]> {
        Self::specs(self)
    }
    fn get(&self, name: &str) -> Option<SharedTool> {
        Self::get(self, name)
    }
}
