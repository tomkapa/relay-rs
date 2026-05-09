// Decode a `ChatMessage` JSONB envelope into pieces the UI can render.
// Backend wire shape: `{role, contents: [{kind, value}]}` — see
// src/provider/chat.rs.

import type { ChatMessageBody } from "../types/api";

export type DecodedBody = {
  text: string;
  reasoning: string;
  toolCalls: { id: string; name: string; input: unknown }[];
  toolResults: { call_id: string; output: string; is_error?: boolean }[];
};

export function decodeBody(body: ChatMessageBody | undefined): DecodedBody {
  const out: DecodedBody = {
    text: "",
    reasoning: "",
    toolCalls: [],
    toolResults: [],
  };
  if (!body) return out;

  if (Array.isArray(body.contents)) {
    for (const c of body.contents) {
      switch (c.kind) {
        case "text":
          out.text += (out.text ? "\n" : "") + c.value;
          break;
        case "reasoning":
          out.reasoning += (out.reasoning ? "\n" : "") + c.value;
          break;
        case "tool_call":
          out.toolCalls.push(c.value);
          break;
        case "tool_result":
          out.toolResults.push(c.value);
          break;
      }
    }
    return out;
  }

  if (typeof body.content === "string") out.text = body.content;
  return out;
}
