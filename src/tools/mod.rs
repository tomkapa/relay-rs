//! Tool subsystem.
//!
//! A `Tool` is anything the model can invoke. Implementations register a name, a
//! human-readable description, a JSON schema for inputs, and an `execute` method that
//! turns a `Value` input into a `String` output (success or error).
//!
//! The agent never imports concrete tools directly — it pulls a [`ToolRegistry`] off the
//! composition root. Adding a tool is one new file plus one line in `app.rs`.

pub mod limits;
mod registry;
mod toolbox;
mod traits;
mod url;
mod web_fetch;
mod web_search;

pub use limits::{TOOL_RESULT_MAX_BYTES, truncate_to_char_boundary};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use toolbox::{DynamicToolSource, ToolBox};
pub use traits::{SharedTool, Tool, ToolError};
pub use url::{FetchUrl, UrlError};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
