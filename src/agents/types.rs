//! Domain types for the agents subsystem.
//!
//! CLAUDE.md §1: every value carrying an invariant gets a newtype with a `TryFrom`
//! smart constructor. The HTTP boundary parses raw JSON into these types once;
//! nothing downstream constructs them directly.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::ParseError;

use super::limits::{AGENT_NAME_MAX_LEN, AGENT_SYSTEM_PROMPT_MAX_LEN};

crate::uuid_newtype! {
    /// Opaque identifier for a registered agent row. Wire format and DB column both
    /// use `agent_id`; this is the typed handle.
    pub AgentId
}

/// Operator-chosen display name. Used for logging and operator UIs only — the
/// model never sees the name, only the resolved `system_prompt` text.
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

/// Snapshot of a single row in the `agents` table.
///
/// `reflection_role` is the optional, role-specific guidance the
/// reflection composer (Phase 4) splices into the reflection turn's
/// system prompt. `None` means "use the default reflection-core prompt
/// alone".
#[derive(Debug, Clone)]
pub struct AgentRecord {
    pub id: AgentId,
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
    pub reflection_role: Option<AgentSystemPrompt>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Seed payload used by the init function to insert the default agent row when
/// none exists. Both fields are pre-validated newtypes so the inserter cannot
/// land malformed data.
#[derive(Debug, Clone)]
pub struct DefaultAgentSeed {
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
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
}
