//! Domain types for the agents subsystem.
//!
//! CLAUDE.md §1: every value carrying an invariant gets a newtype with a `TryFrom`
//! smart constructor. The HTTP boundary parses raw JSON into these types once;
//! nothing downstream constructs them directly.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::auth::OrgId;
use crate::mcp::McpServerId;
use crate::types::ParseError;

use super::limits::{
    AGENT_DESCRIPTION_MAX_LEN, AGENT_NAME_MAX_LEN, AGENT_SYSTEM_PROMPT_MAX_LEN,
    MAX_ALLOWED_MCP_SERVERS_PER_AGENT,
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
/// `allowed_mcp_servers` is the per-agent MCP allowlist: every id in this
/// vector grants the agent visibility to that server's tools. The semantics
/// are strict — an empty vector means **zero** MCP tools, not "all of them".
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
    pub allowed_mcp_servers: AllowedMcpServers,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Bounded list of MCP server ids an agent is allowed to see.
///
/// The newtype enforces the cardinality cap on every construction path
/// (HTTP, store reload, factory wiring). Empty is a legitimate value — the
/// "no MCP tools" lockdown — and is the default for a freshly minted agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedMcpServers(Vec<McpServerId>);

impl AllowedMcpServers {
    #[must_use]
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn as_slice(&self) -> &[McpServerId] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn contains(&self, id: McpServerId) -> bool {
        self.0.contains(&id)
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<McpServerId> {
        self.0
    }
}

impl TryFrom<Vec<McpServerId>> for AllowedMcpServers {
    type Error = ParseError;

    fn try_from(raw: Vec<McpServerId>) -> Result<Self, Self::Error> {
        if raw.len() > MAX_ALLOWED_MCP_SERVERS_PER_AGENT {
            return Err(ParseError::OutOfRange {
                field: "allowed_mcp_servers",
                detail: "too many entries",
            });
        }
        Ok(Self(raw))
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
    fn allowed_mcp_servers_default_is_empty() {
        let a = AllowedMcpServers::default();
        assert!(a.is_empty());
        assert_eq!(a.len(), 0);
        assert_eq!(a.as_slice(), &[]);
    }

    #[test]
    fn allowed_mcp_servers_rejects_oversize() {
        let too_many: Vec<McpServerId> = (0..=MAX_ALLOWED_MCP_SERVERS_PER_AGENT)
            .map(|_| McpServerId::new())
            .collect();
        let err = AllowedMcpServers::try_from(too_many).expect_err("over cap");
        assert!(matches!(
            err,
            ParseError::OutOfRange {
                field: "allowed_mcp_servers",
                ..
            }
        ));
    }

    #[test]
    fn allowed_mcp_servers_accepts_at_cap() {
        let at_cap: Vec<McpServerId> = (0..MAX_ALLOWED_MCP_SERVERS_PER_AGENT)
            .map(|_| McpServerId::new())
            .collect();
        let allowed = AllowedMcpServers::try_from(at_cap.clone()).expect("at cap");
        assert_eq!(allowed.len(), MAX_ALLOWED_MCP_SERVERS_PER_AGENT);
        assert!(allowed.contains(at_cap[0]));
    }
}
