//! Tool-subsystem invariants. Per CLAUDE.md §5: every magic number is named, exported,
//! and doc-commented with the *why*.

use std::time::Duration;

/// Per-call timeout for `web_fetch`.
///
/// Most useful pages return in under 5 s; 20 s tolerates the long tail without letting
/// one bad host stall an entire agent turn.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Hard ceiling on bytes returned to the model from a single fetch. Anthropic charges by
/// token; 200 KB is roughly 50 K tokens of plain English — already excessive context.
pub const FETCH_MAX_BODY_BYTES: usize = 200 * 1024;

/// Maximum HTTP redirect hops for `web_fetch`. Defends against redirect loops and against
/// SSRF via redirect to an internal target after the initial URL passes our guard.
pub const FETCH_MAX_REDIRECTS: usize = 5;

/// Per-call timeout for `web_search`. Brave usually responds well under 2 s; 15 s caps
/// pathological cases without locking up an agent turn.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Default and maximum result counts for `web_search`. Brave supports up to 20; we cap
/// at 10 to keep tool output budget modest by default.
pub const SEARCH_DEFAULT_COUNT: u8 = 5;
pub const SEARCH_MAX_COUNT: u8 = 10;

/// Hard ceiling on bytes a tool may return as a single result. Stops a future poorly
/// behaved tool from filling the model context with megabytes.
pub const TOOL_RESULT_MAX_BYTES: usize = 256 * 1024;

// §5: per-tool body caps must always fit within the global tool-result cap so the agent
// boundary doesn't have to truncate something we already truncated upstream.
const _: () = assert!(FETCH_MAX_BODY_BYTES <= TOOL_RESULT_MAX_BYTES);

/// Truncate `s` to at most `target` bytes, on a UTF-8 boundary.
///
/// `String::truncate` panics if the cut lands mid-codepoint; this walks back to the
/// nearest boundary first. Used wherever we cap a tool result against a byte budget
/// (§5).
pub fn truncate_to_char_boundary(s: &mut String, target: usize) {
    let mut cut = target.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
}

#[cfg(test)]
mod tests {
    use super::truncate_to_char_boundary;

    #[test]
    fn ascii_string_truncates_to_target() {
        let mut s = "hello world".to_owned();
        truncate_to_char_boundary(&mut s, 5);
        assert_eq!(s, "hello");
    }

    #[test]
    fn target_past_end_is_a_noop() {
        let mut s = "hi".to_owned();
        truncate_to_char_boundary(&mut s, 100);
        assert_eq!(s, "hi");
    }

    #[test]
    fn multi_byte_codepoint_is_not_split() {
        // "héllo" — 'é' is 2 bytes (0xC3 0xA9). Cutting at 2 lands mid-codepoint.
        let mut s = "héllo".to_owned();
        truncate_to_char_boundary(&mut s, 2);
        assert_eq!(s, "h");
    }
}
