//! Per-agent view of the MCP tool catalogue.
//!
//! Strict-by-default: a server id absent from the allowlist exposes none of
//! that server's tools. Within an allowed server, the value carried by the
//! [`AllowedMcpTools`] map decides whether every tool is exposed
//! ([`ToolScope::All`]) or only a named subset ([`ToolScope::Some`]).

use std::sync::Arc;

use crate::agents::{AllowedMcpTools, ToolScope};
use crate::provider::ToolSpec;
use crate::tools::{DynamicToolSource, SharedTool};

use super::registry::McpRegistry;
use super::types::{McpServerId, McpToolRemoteName};

#[derive(Debug, Clone)]
pub struct ScopedMcpSource {
    registry: McpRegistry,
    allowed: AllowedMcpTools,
}

impl ScopedMcpSource {
    #[must_use]
    pub fn new(registry: McpRegistry, allowed: &AllowedMcpTools) -> Self {
        Self {
            registry,
            allowed: allowed.clone(),
        }
    }

    fn permits(&self, server: McpServerId, remote: &McpToolRemoteName) -> bool {
        match self.allowed.tools_for(server) {
            ToolScope::None => false,
            ToolScope::All => true,
            ToolScope::Some(set) => set.contains(remote),
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
            if let Some(origin) = snapshot.tool_origins.get(spec.name.as_str())
                && self.permits(origin.server, &origin.remote_name)
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
        let (tool, origin) = self.registry.lookup(name)?;
        self.permits(origin.server, &origin.remote_name)
            .then_some(tool)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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

    fn spec_names(specs: &[crate::provider::ToolSpec]) -> Vec<String> {
        specs.iter().map(|s| s.name.as_str().to_owned()).collect()
    }

    /// Build an [`AllowedMcpTools`] from a list of `(server, scope)` entries
    /// where `scope = None` means "all tools" and `scope = Some(&[..])`
    /// means "only these remote names."
    fn allow(entries: &[(McpServerId, Option<&[&str]>)]) -> AllowedMcpTools {
        let mut raw: BTreeMap<McpServerId, Option<Vec<String>>> = BTreeMap::new();
        for (id, names) in entries {
            let v = names.map(|list| list.iter().map(|s| (*s).to_owned()).collect());
            raw.insert(*id, v);
        }
        AllowedMcpTools::try_from(raw).expect("valid scope")
    }

    #[test]
    fn empty_allowlist_hides_every_tool() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s1, "beta".into(), fake("mcp_one_beta")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &AllowedMcpTools::empty());
        assert!(scoped.specs().is_empty());
        assert!(scoped.get("mcp_one_alpha").is_none());
    }

    #[test]
    fn allowed_server_with_none_scope_exposes_every_tool() {
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s2, "beta".into(), fake("mcp_two_beta")),
            (s2, "gamma".into(), fake("mcp_two_gamma")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s2, None)]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "mcp_two_beta"));
        assert!(names.iter().any(|n| n == "mcp_two_gamma"));
        assert!(scoped.get("mcp_one_alpha").is_none());
        assert!(scoped.get("mcp_two_beta").is_some());
    }

    #[test]
    fn partial_scope_keeps_only_named_tools() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s1, "beta".into(), fake("mcp_one_beta")),
            (s1, "gamma".into(), fake("mcp_one_gamma")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, Some(&["alpha", "gamma"]))]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "mcp_one_alpha"));
        assert!(names.iter().any(|n| n == "mcp_one_gamma"));
        assert!(scoped.get("mcp_one_beta").is_none());
    }

    #[test]
    fn empty_subset_locks_down_an_allowed_server() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s1, "beta".into(), fake("mcp_one_beta")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, Some(&[]))]));
        assert!(scoped.specs().is_empty());
        assert!(scoped.get("mcp_one_alpha").is_none());
    }

    #[test]
    fn unknown_tool_yields_none_even_when_allowlist_nonempty() {
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![(
            s1,
            "alpha".into(),
            fake("mcp_one_alpha"),
        )]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, None)]));
        assert!(scoped.get("mcp_one_does_not_exist").is_none());
    }

    #[test]
    fn allowing_multiple_servers_unions_their_tools() {
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let s3 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s2, "beta".into(), fake("mcp_two_beta")),
            (s3, "gamma".into(), fake("mcp_three_gamma")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, None), (s3, None)]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "mcp_one_alpha"));
        assert!(names.iter().any(|n| n == "mcp_three_gamma"));
        assert!(scoped.get("mcp_two_beta").is_none());
    }

    #[test]
    fn dangling_allowlist_id_is_inert() {
        // Operator deleted MCP server s2 but the agent's allowlist still
        // names it. The filter must not error and must not surface
        // anything — the registry no longer carries s2's tools.
        let s1 = McpServerId::new();
        let s2 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![(
            s1,
            "alpha".into(),
            fake("mcp_one_alpha"),
        )]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, None), (s2, None)]));
        let names = spec_names(&scoped.specs());
        assert_eq!(names, vec!["mcp_one_alpha".to_owned()]);
    }

    #[test]
    fn partial_scope_referencing_unknown_remote_name_keeps_zero() {
        // The agent asked for `phantom`, which the live server does not
        // expose. The known tools are not surfaced because they aren't on
        // the per-tool list either.
        let s1 = McpServerId::new();
        let registry = McpRegistry::for_test_with_remote_names(vec![
            (s1, "alpha".into(), fake("mcp_one_alpha")),
            (s1, "beta".into(), fake("mcp_one_beta")),
        ]);
        let scoped = ScopedMcpSource::new(registry, &allow(&[(s1, Some(&["phantom"]))]));
        assert!(scoped.specs().is_empty());
        assert!(scoped.get("mcp_one_alpha").is_none());
    }
}
