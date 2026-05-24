use super::config::{FieldSpec, MatchRule, TranscriptSchema, WatchTarget};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn get_value_by_path(input: &Value, path: &str) -> Option<Value> {
    if path.trim().is_empty() {
        return None;
    }
    let mut current = input;
    for token in parse_path(path) {
        match token {
            PathToken::Key(key) => current = current.get(&key)?,
            PathToken::Index(index) => current = current.get(index)?,
        }
    }
    Some(current.clone())
}

pub fn resolve_field_spec(
    spec: Option<&FieldSpec>,
    entry: &Value,
    ctx: &ResolveContext<'_>,
) -> Option<Value> {
    let spec = spec?;
    match spec {
        FieldSpec::Path(path) => {
            resolve_context_path(path, ctx).or_else(|| get_value_by_path(entry, path))
        }
        FieldSpec::Spec {
            path,
            value,
            coalesce,
            default,
        } => {
            if let Some(candidates) = coalesce {
                for candidate in candidates {
                    let resolved = resolve_field_spec(Some(candidate), entry, ctx);
                    if !is_empty(&resolved) {
                        return resolved;
                    }
                }
            }
            if let Some(path) = path {
                let resolved =
                    resolve_context_path(path, ctx).or_else(|| get_value_by_path(entry, path));
                if !is_empty(&resolved) {
                    return resolved;
                }
            }
            value.clone().or_else(|| default.clone())
        }
    }
}

pub fn resolve_fields(
    fields: &BTreeMap<String, FieldSpec>,
    entry: &Value,
    ctx: &ResolveContext<'_>,
) -> BTreeMap<String, Value> {
    fields
        .iter()
        .filter_map(|(key, spec)| {
            resolve_field_spec(Some(spec), entry, ctx).map(|value| (key.clone(), value))
        })
        .collect()
}

pub fn matches_rule(entry: &Value, rule: Option<&MatchRule>, schema: &TranscriptSchema) -> bool {
    let Some(rule) = rule else {
        return true;
    };
    let path = rule
        .path
        .as_deref()
        .or(schema.event_type_path.as_deref())
        .unwrap_or("type");
    let value = get_value_by_path(entry, path);
    if rule.exists.unwrap_or(false) && is_empty(&value) {
        return false;
    }
    if let Some(expected) = &rule.equals {
        return value.as_ref() == Some(expected);
    }
    if let Some(values) = &rule.in_values {
        return value.as_ref().is_some_and(|value| values.contains(value));
    }
    if let Some(needle) = &rule.contains {
        return value
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(needle));
    }
    if let Some(pattern) = &rule.regex {
        return value
            .as_ref()
            .is_some_and(|value| value.to_string().contains(pattern));
    }
    true
}

pub struct ResolveContext<'a> {
    pub watch: &'a WatchTarget,
    pub schema: &'a TranscriptSchema,
    pub session: Option<&'a BTreeMap<String, Value>>,
}

fn resolve_context_path(path: &str, ctx: &ResolveContext<'_>) -> Option<Value> {
    if let Some(key) = path.strip_prefix("$watch.") {
        return serde_json::to_value(ctx.watch)
            .ok()
            .and_then(|watch| get_value_by_path(&watch, key));
    }
    if let Some(key) = path.strip_prefix("$schema.") {
        return serde_json::to_value(ctx.schema)
            .ok()
            .and_then(|schema| get_value_by_path(&schema, key));
    }
    if let Some(key) = path.strip_prefix("$session.") {
        return ctx.session.and_then(|session| session.get(key).cloned());
    }
    if path == "$cwd" {
        return ctx.watch.workspace.clone().map(Value::String);
    }
    if path == "$project" {
        return ctx.watch.project.clone().map(Value::String);
    }
    None
}

fn is_empty(value: &Option<Value>) -> bool {
    matches!(value, None | Some(Value::Null))
        || value
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(str::is_empty)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathToken {
    Key(String),
    Index(usize),
}

fn parse_path(path: &str) -> Vec<PathToken> {
    let cleaned = path.trim().trim_start_matches("$.").trim_start_matches('$');
    let mut tokens = Vec::new();
    for part in cleaned.split('.') {
        if part.is_empty() {
            continue;
        }
        let mut rest = part;
        while let Some(start) = rest.find('[') {
            let key = &rest[..start];
            if !key.is_empty() {
                tokens.push(PathToken::Key(key.to_owned()));
            }
            let Some(end) = rest[start + 1..].find(']') else {
                rest = "";
                break;
            };
            let index = &rest[start + 1..start + 1 + end];
            if let Ok(index) = index.parse() {
                tokens.push(PathToken::Index(index));
            }
            rest = &rest[start + 2 + end..];
        }
        if !rest.is_empty() {
            tokens.push(PathToken::Key(rest.to_owned()));
        }
    }
    tokens
}
