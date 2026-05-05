//! OpenAI-Chat-Completions backend for [`LlmProvider`](crate::provider::LlmProvider).
//!
//! Talks to any endpoint that speaks the OpenAI Chat Completions wire format — DeepSeek
//! at `https://api.deepseek.com/v1`, Together, Groq, an in-house gateway, etc. All
//! knowledge of `async_openai` types stays inside this module; the rest of the codebase
//! sees only the provider-agnostic chat types.

mod client;
mod convert;

pub use client::OpenAiProvider;
