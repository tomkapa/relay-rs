use std::collections::HashMap;
use std::sync::Arc;

use crate::provider::ToolSpec;
use crate::types::ToolName;

use super::traits::SharedTool;

/// Registry built once at startup, then frozen.
///
/// Holds both the executable handles (`HashMap<ToolName, SharedTool>`) and the
/// precomputed `ToolSpec` slice the provider needs each turn — cached so the agent
/// never re-allocates per turn.
#[derive(Clone, Debug)]
pub struct ToolRegistry {
    by_name: Arc<HashMap<ToolName, SharedTool>>,
    specs: Arc<[ToolSpec]>,
}

impl ToolRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            by_name: Arc::new(HashMap::new()),
            specs: Arc::from(Vec::new()),
        }
    }

    #[must_use]
    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::default()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<SharedTool> {
        self.by_name.get(name).cloned()
    }

    #[must_use]
    pub fn specs(&self) -> Arc<[ToolSpec]> {
        self.specs.clone()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[derive(Default, Debug)]
pub struct ToolRegistryBuilder {
    tools: Vec<SharedTool>,
}

impl ToolRegistryBuilder {
    pub fn register(&mut self, tool: SharedTool) -> &mut Self {
        self.tools.push(tool);
        self
    }

    #[must_use]
    pub fn with(mut self, tool: SharedTool) -> Self {
        self.register(tool);
        self
    }

    #[must_use]
    pub fn build(self) -> ToolRegistry {
        let mut by_name: HashMap<ToolName, SharedTool> = HashMap::with_capacity(self.tools.len());
        let mut specs: Vec<ToolSpec> = Vec::with_capacity(self.tools.len());
        for tool in self.tools {
            let name = tool.name().clone();
            specs.push(ToolSpec {
                name: name.clone(),
                description: Arc::from(tool.description()),
                input_schema: tool.input_schema(),
            });
            let prev = by_name.insert(name.clone(), tool);
            // §6: registering two tools under the same name is a programmer error
            // (a typo at composition root). Crash now rather than silently shadowing.
            assert!(
                prev.is_none(),
                "duplicate tool registration: {}",
                name.as_str()
            );
        }
        specs.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        ToolRegistry {
            by_name: Arc::new(by_name),
            specs: Arc::from(specs),
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::*;
    use crate::tools::traits::{Tool, ToolError};

    #[derive(Debug)]
    struct FakeTool {
        name: ToolName,
        schema: Arc<Value>,
    }

    impl FakeTool {
        fn new(n: &str) -> Self {
            Self {
                name: ToolName::try_from(n).expect("valid name"),
                schema: Arc::new(json!({"type": "object"})),
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
            self.schema.clone()
        }
        async fn execute(&self, _input: Value) -> Result<String, ToolError> {
            Ok("ok".into())
        }
    }

    #[test]
    fn build_sorts_specs_and_caches_lookup() {
        let registry = ToolRegistry::builder()
            .with(Arc::new(FakeTool::new("zeta")))
            .with(Arc::new(FakeTool::new("alpha")))
            .build();
        let specs = registry.specs();
        assert_eq!(specs[0].name.as_str(), "alpha");
        assert_eq!(specs[1].name.as_str(), "zeta");
        assert!(registry.get("alpha").is_some());
    }

    #[test]
    #[should_panic(expected = "duplicate tool registration")]
    fn duplicate_registration_panics() {
        let _ = ToolRegistry::builder()
            .with(Arc::new(FakeTool::new("dup")))
            .with(Arc::new(FakeTool::new("dup")))
            .build();
    }
}
