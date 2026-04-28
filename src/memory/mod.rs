use std::sync::Arc;

use crate::tools::{Tool, ToolRegistry};

const SYSTEM_PROMPT_BASE: &str = "You are Relay, a helpful AI agent. \
    You are concise, accurate, and prefer to verify facts using your tools \
    before answering when the answer is not obvious. \
    When you call a tool, briefly state why before the call. \
    When you have enough information, give the user a clear final answer.";

pub struct MemoryManager {
    tools: ToolRegistry,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self {
            tools: ToolRegistry::new(),
        }
    }

    pub fn with_tools(tools: ToolRegistry) -> Self {
        Self { tools }
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        self.tools.register(tool);
    }

    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.all()
    }

    pub fn system_prompt(&self) -> String {
        if self.tools.is_empty() {
            return SYSTEM_PROMPT_BASE.to_string();
        }

        let mut out = String::from(SYSTEM_PROMPT_BASE);
        out.push_str("\n\nTools available to you:\n");
        for (name, description) in self.tools.descriptions() {
            out.push_str(&format!("- {name}: {description}\n"));
        }
        out
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}
