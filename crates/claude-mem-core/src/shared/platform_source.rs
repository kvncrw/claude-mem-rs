/// Detect what IDE/platform launched the session, from environment / CLI args.
pub fn detect_platform_source() -> &'static str {
    if std::env::var("CURSOR_TRACE_ID").is_ok() || std::env::var("CURSOR_SESSION_ID").is_ok() {
        "cursor"
    } else if std::env::var("GEMINI_CLIENT").is_ok() {
        "gemini-cli"
    } else if std::env::var("WINDSURF").is_ok() {
        "windsurf"
    } else {
        "claude"
    }
}
