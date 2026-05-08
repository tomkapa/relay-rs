//! Tool subsystem.
//!
//! A `Tool` is anything the model can invoke. Implementations register a name, a
//! human-readable description, a JSON schema for inputs, and an `execute` method that
//! turns a `Value` input into a `String` output (success or error).
//!
//! The agent never imports concrete tools directly — it pulls a [`ToolRegistry`] off the
//! composition root. Adding a system tool is one new file in `system/` plus one
//! `.with(...)` line in [`system::register`].

pub mod limits;
mod registry;
pub mod system;
mod toolbox;
mod traits;
mod url;

pub use limits::{TOOL_RESULT_MAX_BYTES, truncate_to_char_boundary};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use toolbox::{DynamicToolSource, ToolBox};
pub use traits::{SharedTool, Tool, ToolCallContext, ToolError};
pub use url::{FetchUrl, UrlError};
