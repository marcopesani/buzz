//! Recover displayable errors from exited agent log tails.
//!
//! Split from `storage.rs` (file-size guard). Log-path I/O stays in storage;
//! this module owns the promotion/parsing of meaningful agent errors and the
//! redaction call that keeps NWC / nsec material out of `last_error`.

use std::path::Path;

use super::redact_secrets_with;
use super::storage::read_log_tail;

/// A meaningful error recovered from an exited agent's log tail.
pub struct AgentLogError {
    /// The full log line, wrapped as `Agent reported error…` for display.
    pub message: String,
    /// JSON-RPC error code parsed from the line's `(code N)` marker, or a
    /// synthetic code for known bare prefixes. `None` for legacy-format
    /// lines that carry no code (or when the code fails to parse as i64).
    pub code: Option<i64>,
}

/// Extract a displayable agent error from a log tail, redacting secrets first.
///
/// `extras` are exact substrings to scrub (longest-first) via the shared
/// [`redact_secrets_with`] — typically the agent's provisioned NWC URI
/// and bare `secret=` value from [`crate::wallet::agent_nwc_redaction_extras`].
/// Callers that have the agent pubkey (e.g. runtime sync) must load those
/// extras and pass them in; this function does not touch the keyring.
pub fn meaningful_agent_error_from_log(path: &Path, extras: &[&str]) -> Option<AgentLogError> {
    let raw = read_log_tail(path, 200).ok()?;
    // Same redaction as the log-read UI path — never surface nsec / NWC URIs
    // (or bare NWC secrets) via last_error → managed-agents.json / frontend.
    let tail = redact_secrets_with(&raw, extras);
    tail.lines().rev().map(str::trim).find_map(|line| {
        // New format: "Agent reported error (code -32002): ..."
        if let Some(rest) = line.strip_prefix("Agent reported error (code ") {
            if let Some(paren_end) = rest.find("): ") {
                let code = rest[..paren_end].parse::<i64>().ok();
                return Some(AgentLogError {
                    message: line.to_string(),
                    code,
                });
            }
        }
        // Legacy format (older buzz-acp builds): "Agent reported error: ..."
        if line.starts_with("Agent reported error:") {
            return Some(AgentLogError {
                message: line.to_string(),
                code: None,
            });
        }
        // Bare prefixes emitted by older agent binaries whose Display still leaks
        // unwrapped errors. Promote these so they surface instead of the generic
        // "harness exited with status N" fallback.
        if line.starts_with("llm auth:") {
            return Some(AgentLogError {
                message: format!("Agent reported error: {line}"),
                code: Some(-32001),
            });
        }
        if line.starts_with("llm model not found:") {
            return Some(AgentLogError {
                message: format!("Agent reported error: {line}"),
                code: Some(-32002),
            });
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::NamedTempFile;

    use super::meaningful_agent_error_from_log;

    fn write_log(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temp log");
        file.write_all(content.as_bytes()).expect("write log");
        file
    }

    #[test]
    fn meaningful_agent_error_from_log_promotes_wrapped_llm_auth() {
        let file = write_log(
            "noise\nAgent reported error (code -32001): llm auth: 401 unauthorized: ...\n",
        );
        let result = meaningful_agent_error_from_log(file.path(), &[]).unwrap();
        assert!(result.message.contains("llm auth"));
        assert_eq!(result.code, Some(-32001));
    }

    #[test]
    fn meaningful_agent_error_from_log_promotes_unwrapped_llm_auth() {
        let file = write_log("noise\nllm auth: denied\n");
        let result = meaningful_agent_error_from_log(file.path(), &[]).unwrap();
        assert_eq!(result.message, "Agent reported error: llm auth: denied");
        assert_eq!(result.code, Some(-32001));
    }

    #[test]
    fn meaningful_agent_error_from_log_promotes_bare_model_not_found() {
        let file = write_log("noise\nllm model not found: (some-model) 404\n");
        let result = meaningful_agent_error_from_log(file.path(), &[]).unwrap();
        assert_eq!(
            result.message,
            "Agent reported error: llm model not found: (some-model) 404"
        );
        assert_eq!(result.code, Some(-32002));
    }

    #[test]
    fn meaningful_agent_error_from_log_promotes_legacy_format() {
        let file = write_log("noise\nAgent reported error: llm: 500 internal\n");
        let result = meaningful_agent_error_from_log(file.path(), &[]).unwrap();
        assert_eq!(result.message, "Agent reported error: llm: 500 internal");
        assert_eq!(result.code, None);
    }

    #[test]
    fn meaningful_agent_error_from_log_does_not_promote_midline_auth_text() {
        let file = write_log("noise before llm auth: denied\n");
        assert!(meaningful_agent_error_from_log(file.path(), &[]).is_none());
    }

    /// Regression: a bare NWC secret hex in the log must not reach `last_error`
    /// (which is persisted to managed-agents.json). Callers pass keyring-derived
    /// extras — empty extras would leave the bare secret intact after prefix scrub.
    #[test]
    fn meaningful_agent_error_from_log_redacts_bare_nwc_secret_via_extras() {
        let secret = "deadbeefcafebabe0123456789abcdef";
        let file = write_log(&format!(
            "noise\nAgent reported error (code -32000): leaked secret={secret}\n"
        ));
        let result = meaningful_agent_error_from_log(file.path(), &[secret]).expect("promoted");
        assert!(
            !result.message.contains(secret),
            "bare secret leaked into last_error message: {}",
            result.message
        );
        assert!(result.message.contains("[REDACTED]"));
        assert_eq!(result.code, Some(-32000));
    }
}
