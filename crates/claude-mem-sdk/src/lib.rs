//! claude-mem-sdk — LLM-facing parser and prompt builders.
//!
//! Zero I/O deps: takes raw LLM output text, returns structured `ParsedObservation`
//! / `ParsedSummary` records. Used by worker agents to extract observations from
//! model responses.

pub mod parser;
pub mod prompts;

pub use parser::{parse_observations, parse_summary, ParsedObservation, ParsedSummary};
pub use prompts::{
    build_continuation_prompt, build_init_prompt, build_observation_prompt, build_summary_prompt,
    ObservationPromptInput, SummaryPromptInput,
};
