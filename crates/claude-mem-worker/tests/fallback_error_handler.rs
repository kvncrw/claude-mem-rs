use claude_mem_worker::agents::fallback_error_handler::{
    is_abort_error_info, is_abort_error_name, should_fallback_to_claude,
    should_fallback_to_claude_info, should_fallback_to_claude_message, ErrorInfo,
    FALLBACK_ERROR_PATTERNS,
};

#[test]
fn fallback_patterns_match_typescript_contract() {
    assert_eq!(FALLBACK_ERROR_PATTERNS.len(), 7);
    for pattern in [
        "429",
        "500",
        "502",
        "503",
        "ECONNREFUSED",
        "ETIMEDOUT",
        "fetch failed",
    ] {
        assert!(FALLBACK_ERROR_PATTERNS.contains(&pattern));
    }
}

#[test]
fn returns_true_for_transient_provider_errors() {
    for message in [
        "Rate limit exceeded: 429",
        "429 Too Many Requests",
        "500 Internal Server Error",
        "Server returned 500",
        "502 Bad Gateway",
        "Upstream returned 502",
        "503 Service Unavailable",
        "Server is 503",
        "connect ECONNREFUSED 127.0.0.1:8080",
        "ECONNREFUSED",
        "connect ETIMEDOUT",
        "Request ETIMEDOUT",
        "fetch failed",
        "fetch failed: network error",
    ] {
        assert!(should_fallback_to_claude_message(message), "{message}");
    }
}

#[test]
fn returns_false_for_non_fallback_errors() {
    for message in [
        "400 Bad Request",
        "400 Invalid argument",
        "401 Unauthorized",
        "403 Forbidden",
        "404 Not Found",
        "Something went wrong",
        "Unknown error",
        "Bad Request",
    ] {
        assert!(!should_fallback_to_claude_message(message), "{message}");
    }
}

#[test]
fn handles_rust_display_and_structured_error_info() {
    assert!(should_fallback_to_claude("429 rate limited"));
    assert!(!should_fallback_to_claude("invalid input"));

    assert!(should_fallback_to_claude_info(Some(&ErrorInfo::message(
        "503 unavailable"
    ))));
    assert!(!should_fallback_to_claude_info(Some(&ErrorInfo::message(
        "ok"
    ))));
    assert!(!should_fallback_to_claude_info(None));

    let object_like = ErrorInfo {
        name: None,
        message: None,
    };
    assert!(!should_fallback_to_claude_info(Some(&object_like)));
    assert!(should_fallback_to_claude(429));
}

#[test]
fn abort_error_detection_uses_error_name_only() {
    assert!(is_abort_error_name("AbortError"));
    assert!(is_abort_error_info(Some(&ErrorInfo::named(
        "AbortError",
        "aborted"
    ))));

    assert!(!is_abort_error_name("TimeoutError"));
    assert!(!is_abort_error_info(Some(&ErrorInfo::named(
        "TimeoutError",
        "timeout"
    ))));
    assert!(!is_abort_error_info(Some(&ErrorInfo::message(
        "AbortError"
    ))));
    assert!(!is_abort_error_info(None));
}
