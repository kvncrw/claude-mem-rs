use std::path::Path;

/// Derive a project name from a directory path (port of
/// `utils/project-name.ts`).
///
/// Rules:
/// - trailing slashes stripped
/// - returns the last path component
/// - `~` collapses to `$HOME`
pub fn project_name_from_cwd(cwd: &str) -> String {
    let cwd = expand_tilde(cwd);
    let stripped = cwd.trim_end_matches('/').trim_end_matches('\\');
    if stripped.is_empty() {
        return String::from("unknown");
    }
    Path::new(stripped)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| String::from("unknown"))
}

fn expand_tilde(input: &str) -> String {
    if input == "~" {
        crate::shared::platform_paths::home_dir()
            .to_string_lossy()
            .into_owned()
    } else if let Some(rest) = input.strip_prefix("~/") {
        let home = crate::shared::platform_paths::home_dir();
        format!("{}/{}", home.to_string_lossy(), rest)
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_last_segment() {
        assert_eq!(project_name_from_cwd("/home/alice/my-app"), "my-app");
    }

    #[test]
    fn strips_trailing_slash() {
        assert_eq!(project_name_from_cwd("/tmp/foo/"), "foo");
    }

    #[test]
    fn handles_tilde_only() {
        std::env::set_var("HOME", "/home/me");
        assert_eq!(project_name_from_cwd("~"), "me");
    }

    #[test]
    fn handles_tilde_prefix() {
        std::env::set_var("HOME", "/home/me");
        assert_eq!(project_name_from_cwd("~/projects/foo"), "foo");
    }
}
