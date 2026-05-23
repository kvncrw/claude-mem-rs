//! Error classification for provider fallback.
//!
//! Port of `src/services/worker/agents/FallbackErrorHandler.ts`.

pub const FALLBACK_ERROR_PATTERNS: [&str; 7] = [
    "429",
    "500",
    "502",
    "503",
    "ECONNREFUSED",
    "ETIMEDOUT",
    "fetch failed",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorInfo {
    pub name: Option<String>,
    pub message: Option<String>,
}

impl ErrorInfo {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            name: None,
            message: Some(message.into()),
        }
    }

    pub fn named(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            message: Some(message.into()),
        }
    }
}

pub fn should_fallback_to_claude(error: impl std::fmt::Display) -> bool {
    should_fallback_to_claude_message(&error.to_string())
}

pub fn should_fallback_to_claude_info(error: Option<&ErrorInfo>) -> bool {
    let Some(error) = error else {
        return false;
    };
    should_fallback_to_claude_message(error.message.as_deref().unwrap_or_default())
}

pub fn should_fallback_to_claude_message(message: &str) -> bool {
    FALLBACK_ERROR_PATTERNS
        .iter()
        .any(|pattern| message.contains(pattern))
}

pub fn is_abort_error_info(error: Option<&ErrorInfo>) -> bool {
    error
        .and_then(|error| error.name.as_deref())
        .is_some_and(is_abort_error_name)
}

pub fn is_abort_error_name(name: &str) -> bool {
    name == "AbortError"
}
