//! Context compiler and injection.
//!
//! Entry: `generate_context(input, for_human)` renders the `<claude-mem-context>`
//! block shown to the model at SessionStart and to the human viewer.

pub mod formatters;
pub mod observation_compiler;
pub mod sections;
pub mod token_calculator;
