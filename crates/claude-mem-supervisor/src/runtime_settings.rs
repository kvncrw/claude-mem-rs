use claude_mem_core::shared::platform_paths::claude_mem_home;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn apply_worker_settings_env() -> usize {
    let path = worker_settings_path();
    let Ok(pairs) = worker_settings_env_pairs(&path, |key| std::env::var_os(key).is_some()) else {
        return 0;
    };
    let count = pairs.len();
    for (key, value) in pairs {
        std::env::set_var(key, value);
    }
    count
}

fn worker_settings_path() -> PathBuf {
    claude_mem_home().join("settings.json")
}

fn worker_settings_env_pairs(
    path: &Path,
    exists: impl Fn(&str) -> bool,
) -> Result<Vec<(String, String)>, serde_json::Error> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    let value = serde_json::from_str::<Value>(&text)?;
    let Some(settings) = value.as_object() else {
        return Ok(Vec::new());
    };
    Ok(settings
        .iter()
        .filter(|(key, _)| is_worker_runtime_key(key))
        .filter(|(key, _)| !exists(key))
        .filter_map(|(key, value)| {
            let value = value.as_str()?.trim();
            (!value.is_empty()).then(|| (key.clone(), value.to_owned()))
        })
        .collect())
}

fn is_worker_runtime_key(key: &str) -> bool {
    key.starts_with("CLAUDE_MEM_")
        || matches!(
            key,
            "CLAUDE_CODE_PATH" | "ANTHROPIC_API_KEY" | "GEMINI_API_KEY" | "OPENROUTER_API_KEY"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn settings_file_supplies_worker_runtime_env_without_overriding_process_env() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "CLAUDE_MEM_PROVIDER": "claude",
              "CLAUDE_MEM_MODEL": "sonnet",
              "CLAUDE_MEM_OPENROUTER_API_KEY": "",
              "OPENROUTER_API_KEY": "sk-test",
              "UNRELATED": "ignored"
            }"#,
        )
        .unwrap();
        let existing = HashSet::from(["CLAUDE_MEM_MODEL"]);

        let pairs = worker_settings_env_pairs(&path, |key| existing.contains(key)).unwrap();

        assert_eq!(
            pairs,
            vec![
                ("CLAUDE_MEM_PROVIDER".to_owned(), "claude".to_owned()),
                ("OPENROUTER_API_KEY".to_owned(), "sk-test".to_owned())
            ]
        );
    }

    #[test]
    fn missing_settings_file_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let pairs =
            worker_settings_env_pairs(&dir.path().join("settings.json"), |_| false).unwrap();
        assert!(pairs.is_empty());
    }
}
