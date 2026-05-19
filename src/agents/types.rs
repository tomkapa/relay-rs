//! Domain types for the agents subsystem.
//!
//! CLAUDE.md §1: every value carrying an invariant gets a newtype with a `TryFrom`
//! smart constructor. The HTTP boundary parses raw JSON into these types once;
//! nothing downstream constructs them directly.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::auth::OrgId;
use crate::mcp::{McpServerId, McpToolRemoteName};
use crate::types::ParseError;

use super::limits::{
    AGENT_DESCRIPTION_MAX_LEN, AGENT_NAME_MAX_LEN, AGENT_SYSTEM_PROMPT_MAX_LEN,
    MAX_ALLOWED_MCP_SERVERS_PER_AGENT, MAX_ALLOWED_MCP_TOOLS_PER_SERVER_PER_AGENT,
};

crate::uuid_newtype! {
    /// Opaque identifier for a registered agent row. Wire format and DB column both
    /// use `agent_id`; this is the typed handle.
    pub AgentId
}

/// Role-shaped agent name (doc/agent_discovery_plan.md §6).
///
/// Globally unique on `lower(name)`; the model addresses peers by this name
/// in `send_message` and `search_agents`, and the renderer surfaces it in
/// the `<agents>` block and in `<memory>` Collaborator entries. The wire
/// label is preserved as-is; case-insensitivity is enforced by the
/// `agents_name_lower_unique` index.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AgentName(Arc<str>);

impl AgentName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AgentName {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(ParseError::Empty {
                field: "agent_name",
            });
        }
        if raw.len() > AGENT_NAME_MAX_LEN {
            return Err(ParseError::TooLong {
                field: "agent_name",
                max: AGENT_NAME_MAX_LEN,
                got: raw.len(),
            });
        }
        Ok(Self(Arc::from(raw)))
    }
}

impl TryFrom<String> for AgentName {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl fmt::Debug for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AgentName").field(&&*self.0).finish()
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AgentName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

/// Validated, non-empty role-specific system prompt. Reference-counted so the
/// memory layer can hand the same allocation to the provider without copying.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentSystemPrompt(Arc<str>);

impl AgentSystemPrompt {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_arc(self) -> Arc<str> {
        self.0
    }
}

impl TryFrom<&str> for AgentSystemPrompt {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.trim().is_empty() {
            return Err(ParseError::Empty {
                field: "agent_system_prompt",
            });
        }
        if raw.len() > AGENT_SYSTEM_PROMPT_MAX_LEN {
            return Err(ParseError::TooLong {
                field: "agent_system_prompt",
                max: AGENT_SYSTEM_PROMPT_MAX_LEN,
                got: raw.len(),
            });
        }
        Ok(Self(Arc::from(raw)))
    }
}

impl TryFrom<String> for AgentSystemPrompt {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl fmt::Debug for AgentSystemPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Length-only Debug — full prompts are large and pollute logs.
        f.debug_tuple("AgentSystemPrompt")
            .field(&self.0.len())
            .finish()
    }
}

/// Operator-curated, model-facing one-sentence blurb describing what the
/// agent is for (doc/agent_discovery_plan.md §5).
///
/// Required, non-empty. Distinct from [`AgentSystemPrompt`]: this is for
/// *being found* (embedded for `search_agents`); the system prompt is for
/// *being the agent*. The two surfaces evolve for different reasons —
/// description is a clean positive statement of role; the system prompt
/// can carry negations, examples, style guidance that hurt embedding
/// quality.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentDescription(Arc<str>);

impl AgentDescription {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_arc(self) -> Arc<str> {
        self.0
    }
}

impl TryFrom<&str> for AgentDescription {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.trim().is_empty() {
            return Err(ParseError::Empty {
                field: "agent_description",
            });
        }
        if raw.len() > AGENT_DESCRIPTION_MAX_LEN {
            return Err(ParseError::TooLong {
                field: "agent_description",
                max: AGENT_DESCRIPTION_MAX_LEN,
                got: raw.len(),
            });
        }
        Ok(Self(Arc::from(raw)))
    }
}

impl TryFrom<String> for AgentDescription {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl fmt::Debug for AgentDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AgentDescription").field(&&*self.0).finish()
    }
}

impl fmt::Display for AgentDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AgentDescription {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentDescription {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

/// Slim `(id, name, description)` projection for the `search_agents` tool.
///
/// Distinct from [`AgentRecord`] so similarity search does not pay the
/// round-trip / decode cost for `system_prompt` (up to 64 KiB), the MCP
/// allowlist, and the timestamp columns.
#[derive(Debug, Clone)]
pub struct AgentCard {
    pub id: AgentId,
    pub name: AgentName,
    pub description: AgentDescription,
}

/// Snapshot of a single row in the `agents` table.
///
/// `allowed_mcp_tools` is the per-agent MCP allowlist with per-server tool
/// granularity: every server id present grants the agent visibility to that
/// server, with the value (`None` = all tools, `Some(set)` = only these
/// remote tool names) narrowing what surfaces. Strict semantics: an absent
/// server id means **zero** tools from that server, not "all of them".
/// Operators must explicitly opt an agent in to each server.
#[derive(Debug, Clone)]
pub struct AgentRecord {
    pub id: AgentId,
    /// Owning organisation. Set at insert time from the request principal
    /// (HTTP create) or the calling agent's org (tool-driven create);
    /// required because `agents.org_id` is `NOT NULL`.
    pub org_id: OrgId,
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
    pub description: AgentDescription,
    pub is_default: bool,
    pub allowed_mcp_tools: AllowedMcpTools,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-server tool-allowlist view used by the runtime filter to decide
/// whether a single tool surfaces to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolScope<'a> {
    /// Server is not in the allowlist — every tool it exposes is hidden.
    None,
    /// Server is allowed and every tool it exposes is exposed.
    All,
    /// Server is allowed; only the listed remote names are exposed. May be
    /// empty, which is a valid "server present, lockdown all its tools"
    /// state — distinct from [`ToolScope::None`] only in that an operator
    /// has explicitly opted in to the server but listed zero tools.
    Some(&'a BTreeSet<McpToolRemoteName>),
}

/// Per-agent MCP allowlist with per-server tool granularity.
///
/// Storage: `BTreeMap<McpServerId, Option<BTreeSet<McpToolRemoteName>>>`.
/// `None` value = "all tools from this server"; `Some(set)` = "only these
/// remote names." An absent server id = no access to that server (strict).
///
/// The newtype enforces both caps on every construction path (HTTP, store
/// reload, factory wiring):
/// - at most [`MAX_ALLOWED_MCP_SERVERS_PER_AGENT`] keys
/// - at most [`MAX_ALLOWED_MCP_TOOLS_PER_SERVER_PER_AGENT`] entries per
///   value list (and each remote name is itself bounded via
///   `McpToolRemoteName::try_from`).
///
/// Empty (`{}`) is a legitimate value — the "no MCP tools" lockdown — and
/// is the default for a freshly minted agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedMcpTools(BTreeMap<McpServerId, Option<BTreeSet<McpToolRemoteName>>>);

impl AllowedMcpTools {
    #[must_use]
    pub fn empty() -> Self {
        Self(BTreeMap::new())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of distinct servers in the allowlist (i.e. distinct top-level
    /// keys).
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterator over `(server_id, ToolScope)` for the runtime filter.
    pub fn iter(&self) -> impl Iterator<Item = (McpServerId, ToolScope<'_>)> {
        self.0.iter().map(|(id, v)| {
            let scope = v.as_ref().map_or(ToolScope::All, ToolScope::Some);
            (*id, scope)
        })
    }

    /// Look up the scope for `server`. Returns [`ToolScope::None`] for a
    /// server that's absent from the allowlist.
    #[must_use]
    pub fn tools_for(&self, server: McpServerId) -> ToolScope<'_> {
        match self.0.get(&server) {
            None => ToolScope::None,
            Some(None) => ToolScope::All,
            Some(Some(set)) => ToolScope::Some(set),
        }
    }

    /// True iff this allowlist mentions `server` at all (regardless of
    /// whether the tool subset is `None` or `Some`).
    #[must_use]
    pub fn contains_server(&self, server: McpServerId) -> bool {
        self.0.contains_key(&server)
    }
}

impl Serialize for AllowedMcpTools {
    // Delegates to the inner `BTreeMap`'s own `Serialize`. `Option`,
    // `BTreeSet`, and `McpToolRemoteName` all already implement
    // `Serialize`, so this emits the wire-shaped JSONB without copying
    // any `Arc<str>` into a `String`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AllowedMcpTools {
    // Parses the wire-shaped `BTreeMap<server, Option<Vec<String>>>` and
    // funnels through `TryFrom` so the caps + per-name + uniqueness
    // checks fire on every boundary cross (HTTP, sqlx JSONB, tool
    // input). Boundary error → serde error → HTTP 400 / store backend
    // error, same as every other newtype in this crate.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <BTreeMap<McpServerId, Option<Vec<String>>>>::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<BTreeMap<McpServerId, Option<Vec<String>>>> for AllowedMcpTools {
    type Error = ParseError;

    fn try_from(raw: BTreeMap<McpServerId, Option<Vec<String>>>) -> Result<Self, Self::Error> {
        if raw.len() > MAX_ALLOWED_MCP_SERVERS_PER_AGENT {
            return Err(ParseError::OutOfRange {
                field: "allowed_mcp_tools",
                detail: "too many servers",
            });
        }
        let mut out: BTreeMap<McpServerId, Option<BTreeSet<McpToolRemoteName>>> = BTreeMap::new();
        for (id, names) in raw {
            let scope = match names {
                None => None,
                Some(list) => {
                    if list.len() > MAX_ALLOWED_MCP_TOOLS_PER_SERVER_PER_AGENT {
                        return Err(ParseError::OutOfRange {
                            field: "allowed_mcp_tools",
                            detail: "too many tools for one server",
                        });
                    }
                    let mut set: BTreeSet<McpToolRemoteName> = BTreeSet::new();
                    for raw_name in list {
                        let name = McpToolRemoteName::try_from(raw_name)?;
                        if !set.insert(name) {
                            return Err(ParseError::Malformed {
                                field: "allowed_mcp_tools",
                                detail: "duplicate tool name in server list",
                            });
                        }
                    }
                    Some(set)
                }
            };
            out.insert(id, scope);
        }
        Ok(Self(out))
    }
}

/// Seed payload used by the init function to insert the default agent row when
/// none exists. Every field is a pre-validated newtype so the inserter cannot
/// land malformed data.
#[derive(Debug, Clone)]
pub struct DefaultAgentSeed {
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
    pub description: AgentDescription,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_rejects_empty_and_oversize() {
        assert!(AgentName::try_from("").is_err());
        let big = "a".repeat(AGENT_NAME_MAX_LEN + 1);
        assert!(AgentName::try_from(big.as_str()).is_err());
    }

    #[test]
    fn agent_name_accepts_normal() {
        let n = AgentName::try_from("assistant").expect("valid");
        assert_eq!(n.as_str(), "assistant");
    }

    #[test]
    fn agent_system_prompt_rejects_empty_and_whitespace() {
        assert!(AgentSystemPrompt::try_from("").is_err());
        assert!(AgentSystemPrompt::try_from("   \n\t").is_err());
    }

    #[test]
    fn agent_system_prompt_rejects_oversize() {
        let big = "a".repeat(AGENT_SYSTEM_PROMPT_MAX_LEN + 1);
        assert!(AgentSystemPrompt::try_from(big.as_str()).is_err());
    }

    #[test]
    fn agent_system_prompt_accepts_normal() {
        let p = AgentSystemPrompt::try_from("be helpful").expect("valid");
        assert_eq!(p.as_str(), "be helpful");
    }

    #[test]
    fn allowed_mcp_tools_default_is_empty() {
        let a = AllowedMcpTools::default();
        assert!(a.is_empty());
        assert_eq!(a.len(), 0);
    }

    #[test]
    fn allowed_mcp_tools_rejects_too_many_servers() {
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        for _ in 0..=MAX_ALLOWED_MCP_SERVERS_PER_AGENT {
            raw.insert(McpServerId::new(), None);
        }
        let err = AllowedMcpTools::try_from(raw).expect_err("over server cap");
        assert!(matches!(
            err,
            ParseError::OutOfRange {
                field: "allowed_mcp_tools",
                detail: "too many servers",
            }
        ));
    }

    #[test]
    fn allowed_mcp_tools_rejects_too_many_tools_per_server() {
        let id = McpServerId::new();
        let mut list: Vec<String> = Vec::new();
        for i in 0..=MAX_ALLOWED_MCP_TOOLS_PER_SERVER_PER_AGENT {
            list.push(format!("t{i}"));
        }
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        raw.insert(id, Some(list));
        let err = AllowedMcpTools::try_from(raw).expect_err("over tools cap");
        assert!(matches!(
            err,
            ParseError::OutOfRange {
                field: "allowed_mcp_tools",
                detail: "too many tools for one server",
            }
        ));
    }

    #[test]
    fn allowed_mcp_tools_rejects_empty_tool_name() {
        let id = McpServerId::new();
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        raw.insert(id, Some(vec![String::new()]));
        let err = AllowedMcpTools::try_from(raw).expect_err("empty name");
        assert!(matches!(err, ParseError::Empty { .. }));
    }

    #[test]
    fn allowed_mcp_tools_rejects_duplicate_tool_in_one_server_list() {
        let id = McpServerId::new();
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        raw.insert(id, Some(vec!["a".into(), "a".into()]));
        let err = AllowedMcpTools::try_from(raw).expect_err("dup");
        assert!(matches!(
            err,
            ParseError::Malformed {
                field: "allowed_mcp_tools",
                ..
            }
        ));
    }

    #[test]
    fn allowed_mcp_tools_distinguishes_all_versus_some_empty() {
        let id_all = McpServerId::new();
        let id_some_empty = McpServerId::new();
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        raw.insert(id_all, None);
        raw.insert(id_some_empty, Some(Vec::new()));
        let allowed = AllowedMcpTools::try_from(raw).expect("valid");
        assert!(matches!(allowed.tools_for(id_all), ToolScope::All));
        let empty_set = match allowed.tools_for(id_some_empty) {
            ToolScope::Some(set) => set,
            other => panic!("expected Some(empty), got {other:?}"),
        };
        assert!(empty_set.is_empty());
        let unknown = McpServerId::new();
        assert!(matches!(allowed.tools_for(unknown), ToolScope::None));
    }

    #[test]
    fn allowed_mcp_tools_accepts_at_caps() {
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        for i in 0..MAX_ALLOWED_MCP_SERVERS_PER_AGENT {
            let id = McpServerId::new();
            let tools: Vec<String> = (0..MAX_ALLOWED_MCP_TOOLS_PER_SERVER_PER_AGENT)
                .map(|t| format!("s{i}_t{t}"))
                .collect();
            raw.insert(id, Some(tools));
        }
        let allowed = AllowedMcpTools::try_from(raw).expect("at caps");
        assert_eq!(allowed.len(), MAX_ALLOWED_MCP_SERVERS_PER_AGENT);
    }
}
