//! Agent vs. human formatters.

use crate::types::ObservationRow;

pub struct FormatOptions {
    pub for_human: bool,
    pub max_narrative_chars: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            for_human: false,
            max_narrative_chars: 400,
        }
    }
}

pub fn format_observation(obs: &ObservationRow, opts: &FormatOptions) -> String {
    let header = format!(
        "#{} {}{}",
        obs.id,
        obs.title.as_deref().unwrap_or("(untitled)"),
        obs.created_at,
    );
    let mut body = String::new();
    if let Some(nar) = obs.narrative.as_deref() {
        let truncated = if opts.for_human {
            nar
        } else if nar.len() > opts.max_narrative_chars {
            &nar[..opts.max_narrative_chars]
        } else {
            nar
        };
        body.push_str(truncated);
    }
    format!("{}\n{}", header, body)
}
