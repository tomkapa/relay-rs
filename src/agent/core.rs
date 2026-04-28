use claudius::{
    ContentBlock, KnownModel, Message, MessageCreateParams, MessageParam, MessageRole, Model,
    ToolResultBlock, ToolUnionParam, ToolUseBlock,
};
use tracing::{info, warn};

use super::error::AgentError;
use super::limits::{DEFAULT_MAX_TOKENS, DEFAULT_MAX_TURNS};
use crate::app::AppState;
use crate::memory::MemoryManager;

pub struct Agent {
    state: AppState,
    memory: MemoryManager,
    model: Model,
    max_tokens: u32,
    max_turns: u32,
}

impl Agent {
    pub fn new(state: AppState, memory: MemoryManager) -> Self {
        Self {
            state,
            memory,
            model: Model::Known(KnownModel::ClaudeSonnet45),
            max_tokens: DEFAULT_MAX_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
        }
    }

    pub fn with_model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub async fn reply(&self, prompt: &str) -> Result<String, AgentError> {
        let registered = self.memory.tools();
        let tool_params: Vec<ToolUnionParam> =
            registered.iter().map(|t| (&**t).into()).collect();
        let system_prompt = self.memory.system_prompt();

        let mut messages = vec![MessageParam::user(prompt.to_string())];

        for turn in 0..self.max_turns {
            let mut params = MessageCreateParams::new(
                self.max_tokens,
                messages.clone(),
                self.model.clone(),
            )
            .with_system_string(system_prompt.clone());

            if !tool_params.is_empty() {
                params = params.with_tools(tool_params.clone());
            }

            info!(turn, "agent.turn.start");
            let response: Message = self.state.anthropic().send(params).await?;
            log_response_blocks(&response.content);

            let tool_uses: Vec<&ToolUseBlock> = response
                .content
                .iter()
                .filter_map(ContentBlock::as_tool_use)
                .collect();

            if tool_uses.is_empty() {
                let text = collect_text(&response.content);
                if text.is_empty() {
                    return Err(AgentError::EmptyReply);
                }
                info!(turn, "agent.turn.final");
                return Ok(text);
            }

            messages.push(MessageParam::new_with_blocks(
                response.content.clone(),
                MessageRole::Assistant,
            ));

            let mut result_blocks = Vec::with_capacity(tool_uses.len());
            for use_block in tool_uses {
                let result = self.run_tool(use_block).await;
                result_blocks.push(ContentBlock::ToolResult(result));
            }
            messages.push(MessageParam::new_with_blocks(result_blocks, MessageRole::User));
        }

        Err(AgentError::MaxTurnsExceeded(self.max_turns))
    }

    async fn run_tool(&self, use_block: &ToolUseBlock) -> ToolResultBlock {
        let id = use_block.id.clone();
        let Some(tool) = self.memory.get_tool(&use_block.name) else {
            warn!(name = %use_block.name, "tool.unknown");
            return ToolResultBlock::new(id)
                .with_string_content(format!("unknown tool: {}", use_block.name))
                .with_error(true);
        };

        match tool.execute(use_block.input.clone()).await {
            Ok(output) => {
                info!(name = %use_block.name, bytes = output.len(), "tool.result.ok");
                ToolResultBlock::new(id).with_string_content(output)
            }
            Err(e) => {
                warn!(name = %use_block.name, error = %e, "tool.result.err");
                ToolResultBlock::new(id)
                    .with_string_content(e.to_string())
                    .with_error(true)
            }
        }
    }
}

fn log_response_blocks(blocks: &[ContentBlock]) {
    for block in blocks {
        match block {
            ContentBlock::Thinking(t) => {
                info!(thinking = %t.thinking, "agent.thinking");
            }
            ContentBlock::ToolUse(t) => {
                info!(name = %t.name, input = %t.input, "agent.tool_call");
            }
            ContentBlock::Text(t) => {
                info!(bytes = t.text.len(), "agent.text");
            }
            _ => {}
        }
    }
}

fn collect_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let ContentBlock::Text(t) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&t.text);
        }
    }
    out
}
