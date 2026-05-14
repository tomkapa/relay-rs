//! Per-agent view of the MCP tool catalogue.
//!
//! Strict-by-default: an empty allow-set yields zero tools. There is no
//! "unrestricted" mode — the absence of a server id from the set always
//! means the agent cannot see that server's tools.

use std::collections::HashSet;
use std::sync::Arc;

use crate::agents::AllowedMcpServers;
use crate::provider::ToolSpec;
use crate::tools::{DynamicToolSource, SharedTool};

use super::registry::McpRegistry;
use super::types::McpServerId;

#[derive(Debug, Clone)]
pub struct ScopedMcpSource {
    registry: McpRegistry,
    allowed: HashSet<McpServerId>,
}

impl ScopedMcpSource {
    #[must_use]
    pub fn new(registry: McpRegistry, allowed: &AllowedMcpServers) -> Self {
        Self {
            registry,
            allowed: allowed.as_slice().iter().copied().collect(),
        }
    }
}

impl DynamicToolSource for ScopedMcpSource {
    fn specs(&self) -> Arc<[ToolSpec]> {
        if self.allowed.is_empty() {
            return Arc::default();
        }
        let snapshot = self.registry.snapshot();
        let mut kept: Vec<ToolSpec> = Vec::with_capacity(snapshot.specs.len());
        for spec in snapshot.specs.iter() {
            if let Some(server) = snapshot.tool_servers.get(spec.name.as_str())
                && self.allowed.contains(server)
            {
                kept.push(spec.clone());
            }
        }
        Arc::from(kept)
    }

    fn get(&self, name: &str) -> Option<SharedTool> {
        if self.allowed.is_empty() {
            return None;
        }
        let (tool, server) = self.registry.lookup(name)?;
        self.allowed.contains(&server).then_some(tool)
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::*;
    use crate::tools::{Tool, ToolCallContext, ToolError};
    use crate::types::ToolName;

    #[derive(Debug)]
    struct FakeTool {
        name: ToolName,
    }

    impl FakeTool {
        fn new(name: &str) -> Self {
            Self {
                name: ToolName::try_from(name).expect("valid"),
            }
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &ToolName {
            &self.name
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn input_schema(&self) -> Arc<Value> {
            Arc::new(json!({"type":"object"}))
        }
        async fn execute(
            &self,
            _input: Value,
            _ctx: &ToolCallContext,
        ) -> Result<String, ToolError> {
            Ok("ok".into())
        }
    }

    fn fake(name: &str) -> SharedTool {
        Arc::new(FakeTool::new(name))
    }

    fn allow(ids: &[McpServerId]) -> AllowedMcpServers {
        AllowedMcpServers::try_from(ids.to_vec()).expect("under cap")
    }

    fn spec_names(specs: &[crate::provider::ToolSpec]) -> Vec<String> {
        specs.iter().map(|s| s.name.as_str().to_owned()).collect()
    }

    #[test]
    fn empty_allowlist_hides_every_tool() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test(vec![
            (s1, fake("mcp_one_alpha")),
            (s1, fake("mcp_one_beta")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &AllowedMcpServers::empty());
        assert!(scoped.specs().is_empty());
        assert!(scoped.get("mcp_one_alpha").is_none());
    }

    #[test]
    fn allowed_server_exposes_only_its_tools() {
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let registry = McpRegistry::for_test(vec![
            (s1, fake("mcp_one_alpha")),
            (s2, fake("mcp_two_beta")),
            (s2, fake("mcp_two_gamma")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[s2]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "mcp_two_beta"));
        assert!(names.iter().any(|n| n == "mcp_two_gamma"));
        assert!(scoped.get("mcp_one_alpha").is_none());
        assert!(scoped.get("mcp_two_beta").is_some());
    }

    #[test]
    fn unknown_tool_yields_none_even_when_allowlist_nonempty() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test(vec![(s1, fake("mcp_one_alpha"))]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[s1]));
        assert!(scoped.get("mcp_one_does_not_exist").is_none());
    }

    #[test]
    fn allowing_multiple_servers_unions_their_tools() {
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let s3 = McpServerId::new();
        let registry = McpRegistry::for_test(vec![
            (s1, fake("mcp_one_alpha")),
            (s2, fake("mcp_two_beta")),
            (s3, fake("mcp_three_gamma")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[s1, s3]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "mcp_one_alpha"));
        assert!(names.iter().any(|n| n == "mcp_three_gamma"));
        assert!(scoped.get("mcp_two_beta").is_none());
    }

    #[test]
    fn dangling_allowlist_id_is_inert() {
        // Operator deleted MCP server s2 but the agent's allowlist still
        // names it. The filter must not error and must not surface anything
        // — the registry no longer carries s2's tools.
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let registry = McpRegistry::for_test(vec![(s1, fake("mcp_one_alpha"))]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[s1, s2]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names, vec!["mcp_one_alpha".to_owned()]);
    }
}
