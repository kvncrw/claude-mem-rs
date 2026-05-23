//! Timeline date formatting helpers shared between the context compiler and
//! the worker timeline endpoints.

/// Produce the day label used for grouping timeline rows (e.g. "May 23, 2026").
pub fn format_timeline_date(created_at: &str) -> String {
    created_at.chars().take(10).collect()
}

pub fn strip_microseconds(iso: &str) -> String {
    let mut s = iso.to_string();
    if let Some(dot) = s.rfind('.') {
        if let Some(z) = s[dot..].find('Z') {
            s.replace_range(dot..dot + z, "");
        }
    }
    s
}
