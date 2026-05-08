//! End-to-end test for `send_message(Human, ...)` and the
//! `ResponseChunk::AgentMessage` it publishes on the root SSE stream
//! (build step §6).
//!
//! Setup: scripted provider that, on the very first turn, emits a
//! `send_message` tool call addressed to the human, and on the second turn
//! ends the turn with text (which the model intends as a private thought —
//! the spec is explicit that delivery happens via `send_message`, not via
//! `Done.final_text`).
//!
//! We then enqueue a prompt, subscribe to the SSE stream on the request id
//! (which is the DAG root for a fresh enqueue), and assert that an
//! `AgentMessage` chunk arrives with the expected `from` and `content`.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use relay_rs::provider::{AssistantContent, ChatResponse, StopReason, ToolCall, ToolCallId};
use relay_rs::runtime::queue::PromptQueue as _;
use relay_rs::runtime::{IdempotencyKey, NewPromptRequest, ResponseChunk, StreamEvent};
use relay_rs::types::{Participant, Prompt, ToolName};
use serde_json::json;

mod common;
use common::harness::{ScriptedProvider, build_harness};

fn send_message_call(content: &str) -> ChatResponse {
    ChatResponse {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: ToolCallId::try_from("call-1").expect("id"),
            name: ToolName::try_from("send_message").expect("name"),
            input: json!({
                "receiver": { "kind": "human" },
                "content": content,
            }),
        })],
        stop_reason: StopReason::ToolUse,
        ..Default::default()
    }
}

fn final_text(s: &str) -> ChatResponse {
    ChatResponse {
        content: vec![AssistantContent::Text(s.into())],
        stop_reason: StopReason::EndTurn,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_message_human_publishes_agent_message_on_root_stream() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Turn 1: model calls send_message(Human, "hello human").
        send_message_call("hello human"),
        // Turn 2: model emits final text. send_message_calls > 0 already so
        // the worker accepts the success path — no ping-pong retry.
        final_text("(internal close-out)"),
    ]));
    let h = build_harness(provider).await;

    let request_id = h
        .queue
        .enqueue(NewPromptRequest {
            session: None,
            sender: Participant::Human,
            receiver_agent_id: h.default_agent_id,
            parent_session: None,
            content: Prompt::try_from("ping the agent").expect("prompt"),
            idempotency_key: IdempotencyKey::try_from("k-human").expect("key"),
        })
        .await
        .expect("enqueue")
        .request_id();

    // Subscribe to the SSE stream for this request id (== root). Drain
    // chunks until a terminal one (Done/Error) arrives or we time out.
    let chunks = drain_chunks(&h, request_id, Duration::from_secs(8)).await;

    let agent_msg = chunks
        .iter()
        .find_map(|c| match c {
            ResponseChunk::AgentMessage { from, content } => Some((from, content.clone())),
            _ => None,
        })
        .expect("expected an AgentMessage on the root stream");
    let (from, content) = agent_msg;
    assert_eq!(*from, h.default_agent_id, "AgentMessage records caller id");
    assert_eq!(
        content, "hello human",
        "delivered content matches tool input"
    );

    h.workers.shutdown().await;
}

async fn drain_chunks(
    h: &common::harness::WorkerHarness,
    id: relay_rs::runtime::PromptRequestId,
    deadline: Duration,
) -> Vec<ResponseChunk> {
    let source: relay_rs::runtime::SharedResponseSource = h.hub.clone();
    let mut stream = source.subscribe(id, None).await.expect("subscribe");
    let mut got = Vec::new();
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        let next = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        let Ok(Some(item)) = next else { continue };
        let ev = item.expect("ok");
        if let StreamEvent::Chunk(env) = ev {
            let terminal = env.chunk.is_terminal();
            got.push(env.chunk);
            if terminal {
                return got;
            }
        }
    }
    got
}
