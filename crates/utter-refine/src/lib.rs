//! Transcript post-processing: rules, snippets and LLM refinement.

pub mod rules;
pub use rules::{apply_rules, ReplaceRule};
