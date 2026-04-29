//! End-to-end agent tests against a fake `LlmProvider` and a fake `Tool`.
//!
//! Proves that the agent loop is fully provider-agnostic: nothing about Anthropic,
//! reqwest, or claudius appears here. Plug in any backend and it works.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use relay_rs::agent::AgentBuilder;
use relay_rs::hook::HookChain;
use relay_rs::memory::{SharedMemory, StaticMemory};
use relay_rs::provider::{
    AssistantContent, ChatRequest, ChatResponse, LlmProvider, ProviderError, SharedProvider,
    StopReason, ToolCall, ToolCallId,
};
use relay_rs::session::{InMemorySessionStore, SharedSessionStore};
use relay_rs::tools::{SharedTool, Tool, ToolError, ToolRegistry};
use relay_rs::types::{ModelId, Prompt, ToolName};

/// Provider that returns a pre-scripted sequence of responses, one per turn. Records the
/// requests it sees so tests can assert on them.
#[derive(Debug)]
struct ScriptedProvider {
    script: Vec<ChatResponse>,
    cursor: AtomicUsize,
    seen: std::sync::Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    fn new(script: Vec<ChatResponse>) -> Self {
        Self {
            script,
            cursor: AtomicUsize::new(0),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.cursor.load(Ordering::SeqCst)
    }

    fn last_request(&self) -> ChatRequest {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no requests recorded")
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn send(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen.lock().unwrap().push(request);
        let i = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.script
            .get(i)
            .cloned()
            .ok_or_else(|| ProviderError::Transport("script exhausted".into()))
    }
}

/// Tool that records every input it receives and returns a fixed reply.
#[derive(Debug)]
struct CountingTool {
    name: ToolName,
    schema: Arc<Value>,
    calls: AtomicUsize,
}

impl CountingTool {
    fn new(name: &str) -> Self {
        Self {
            name: ToolName::try_from(name).expect("valid name"),
            schema: Arc::new(json!({"type": "object"})),
            calls: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &ToolName {
        &self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn input_schema(&self) -> Arc<Value> {
        self.schema.clone()
    }
    async fn execute(&self, _input: Value) -> Result<String, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("tool ran".into())
    }
}

fn text_response(s: &str, stop: StopReason) -> ChatResponse {
    ChatResponse {
        content: vec![AssistantContent::Text(s.into())],
        stop_reason: stop,
    }
}

fn tool_call_response(name: &str, id: &str) -> ChatResponse {
    ChatResponse {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: ToolCallId::try_from(id).expect("valid"),
            name: ToolName::try_from(name).expect("valid"),
            input: json!({}),
        })],
        stop_reason: StopReason::ToolUse,
    }
}

fn build(provider: Arc<ScriptedProvider>, tools: Vec<SharedTool>) -> relay_rs::Agent {
    let provider: SharedProvider = provider;
    let sessions: SharedSessionStore = Arc::new(InMemorySessionStore::new());
    let memory: SharedMemory = Arc::new(StaticMemory::new("test prompt"));
    let model = ModelId::try_from("test-model").expect("valid");
    let mut builder = ToolRegistry::builder();
    for t in tools {
        builder.register(t);
    }
    AgentBuilder::new(provider, sessions, memory, model)
        .expect("builder")
        .with_tools(builder.build())
        .with_hooks(HookChain::new())
        .build()
}

#[tokio::test]
async fn returns_text_when_no_tool_call() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "hi back",
        StopReason::EndTurn,
    )]));
    let agent = build(provider.clone(), vec![]);

    let session = agent.start_session().await.expect("session");
    let prompt = Prompt::try_from("hello").expect("prompt");
    let reply = agent
        .reply(session, vec![prompt], CancellationToken::new(), None)
        .await
        .expect("reply");

    assert_eq!(reply, "hi back");
    assert_eq!(provider.calls(), 1);
}

#[tokio::test]
async fn runs_tool_then_returns_text() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_call_response("counter", "call-1"),
        text_response("done", StopReason::EndTurn),
    ]));
    let counter = Arc::new(CountingTool::new("counter"));
    let agent = build(provider.clone(), vec![counter.clone()]);

    let session = agent.start_session().await.expect("session");
    let prompt = Prompt::try_from("use the tool").expect("prompt");
    let reply = agent
        .reply(session, vec![prompt], CancellationToken::new(), None)
        .await
        .expect("reply");

    assert_eq!(reply, "done");
    assert_eq!(counter.count(), 1, "tool should have been invoked once");
    assert_eq!(provider.calls(), 2, "two turns: tool call, then final");
}

#[tokio::test]
async fn unknown_tool_does_not_loop_forever() {
    // Provider asks for a tool that isn't registered, then provides a final answer.
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_call_response("missing-tool", "call-x"),
        text_response("recovered", StopReason::EndTurn),
    ]));
    let agent = build(provider.clone(), vec![]);

    let session = agent.start_session().await.expect("session");
    let prompt = Prompt::try_from("try the missing tool").expect("prompt");
    let reply = agent
        .reply(session, vec![prompt], CancellationToken::new(), None)
        .await
        .expect("reply");

    assert_eq!(reply, "recovered");
}

#[tokio::test]
async fn cancellation_short_circuits() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "never used",
        StopReason::EndTurn,
    )]));
    let agent = build(provider, vec![]);

    let session = agent.start_session().await.expect("session");
    let prompt = Prompt::try_from("cancel me").expect("prompt");
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = agent
        .reply(session, vec![prompt], cancel, None)
        .await
        .expect_err("cancelled");
    matches!(err, relay_rs::AgentError::Cancelled);
}

#[tokio::test]
async fn provider_specs_match_registered_tools() {
    // Sanity that the agent forwards the registry's specs to the provider unchanged.
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "ok",
        StopReason::EndTurn,
    )]));
    let counter = Arc::new(CountingTool::new("counter"));
    let agent = build(provider.clone(), vec![counter]);

    let session = agent.start_session().await.expect("session");
    let prompt = Prompt::try_from("hi").expect("prompt");
    let _ = agent
        .reply(session, vec![prompt], CancellationToken::new(), None)
        .await
        .expect("reply");

    let req = provider.last_request();
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.tools[0].name.as_str(), "counter");
}
