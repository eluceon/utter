//! Transcript post-processing: rules, snippets and LLM refinement.

pub mod rules;
pub use rules::{apply_rules, ReplaceRule};

pub mod snippets;
pub use snippets::{match_snippet, normalize, Snippet};

pub mod prompt;
pub use prompt::build_prompt;

pub mod llm;
pub use llm::{LlmConfig, LlmRefiner};
