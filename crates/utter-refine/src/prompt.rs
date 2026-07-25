//! Builds LLM refinement prompts: a tone-specific system prompt plus the raw
//! transcript as user content. Sending the request over HTTP is out of scope
//! here — see the refinement client that consumes this module's output.

use utter_core::Tone;

const CLEAN_TEMPLATE: &str = include_str!("../prompts/clean.txt");
const FORMAL_TEMPLATE: &str = include_str!("../prompts/formal.txt");
const NOTES_TEMPLATE: &str = include_str!("../prompts/notes.txt");
const CODE_COMMENT_TEMPLATE: &str = include_str!("../prompts/code_comment.txt");

/// Returns the raw prompt template for a tone.
///
/// `Tone::Verbatim` has no refinement template of its own — verbatim
/// transcripts skip refinement entirely upstream — so it falls back to the
/// `Clean` template here. Callers must never reach this with `Verbatim`;
/// `build_prompt` enforces that with a `debug_assert!` before calling in.
fn template_for(tone: Tone) -> &'static str {
    match tone {
        Tone::Verbatim | Tone::Clean => CLEAN_TEMPLATE,
        Tone::Formal => FORMAL_TEMPLATE,
        Tone::Notes => NOTES_TEMPLATE,
        Tone::CodeComment => CODE_COMMENT_TEMPLATE,
    }
}

/// Resolves the `{dictionary}` placeholder line in `template`.
///
/// When `dictionary_terms` is non-empty, the placeholder is filled in with a
/// preferred-spellings line. When empty, the placeholder line is removed
/// along with the blank line that separates it from the previous paragraph,
/// so the template doesn't end with residual blank lines.
fn apply_dictionary(template: &str, dictionary_terms: &[String]) -> String {
    if dictionary_terms.is_empty() {
        template.replace("\n\n{dictionary}\n", "\n")
    } else {
        let terms = dictionary_terms.join(", ");
        template.replace(
            "{dictionary}",
            &format!("Preferred spellings of domain terms: {terms}."),
        )
    }
}

/// Builds the chat messages for a refinement request.
///
/// Returns `(system_prompt, user_content)`. `Tone::Verbatim` never reaches
/// here by contract — verbatim transcripts bypass refinement upstream — and
/// that contract is enforced with a `debug_assert!`; release builds fall
/// back to the `Clean` template instead of panicking.
pub fn build_prompt(
    raw: &str,
    tone: Tone,
    dictionary_terms: &[String],
    language_hint: Option<&str>,
) -> (String, String) {
    debug_assert!(
        tone != Tone::Verbatim,
        "Tone::Verbatim must not reach build_prompt"
    );

    let template = template_for(tone);
    let mut system_prompt = apply_dictionary(template, dictionary_terms)
        .trim_end()
        .to_string();

    if let Some(lang) = language_hint {
        system_prompt.push('\n');
        system_prompt.push_str(&format!("The input language is {lang}. Keep it."));
    }

    (system_prompt, raw.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_placeholder_removed_when_empty() {
        let (system, _) = build_prompt("hi", Tone::Clean, &[], None);
        assert!(!system.contains("{dictionary}"));
        assert!(!system.contains("Preferred spellings"));
        assert!(!system.contains("\n\n\n"));
    }

    #[test]
    fn dictionary_placeholder_filled_when_present() {
        let terms = vec!["SQLite".to_string(), "Rust".to_string()];
        let (system, _) = build_prompt("hi", Tone::Clean, &terms, None);
        assert!(system.contains("Preferred spellings of domain terms: SQLite, Rust."));
    }

    #[test]
    fn language_hint_appended() {
        let (system, _) = build_prompt("hi", Tone::Clean, &[], Some("ru"));
        assert!(system.ends_with("The input language is ru. Keep it."));
    }

    #[test]
    fn no_language_hint_has_no_trailer() {
        let (system, _) = build_prompt("hi", Tone::Clean, &[], None);
        assert!(!system.contains("input language"));
    }

    #[test]
    fn user_content_is_trimmed_raw_transcript() {
        let (_, user) = build_prompt("  hello world  \n", Tone::Clean, &[], None);
        assert_eq!(user, "hello world");
    }

    #[test]
    fn every_tone_produces_its_own_template() {
        let (clean, _) = build_prompt("x", Tone::Clean, &[], None);
        let (formal, _) = build_prompt("x", Tone::Formal, &[], None);
        let (notes, _) = build_prompt("x", Tone::Notes, &[], None);
        let (code_comment, _) = build_prompt("x", Tone::CodeComment, &[], None);
        assert_ne!(clean, formal);
        assert_ne!(clean, notes);
        assert_ne!(clean, code_comment);
        assert_ne!(formal, notes);
        assert_ne!(formal, code_comment);
        assert_ne!(notes, code_comment);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Tone::Verbatim must not reach build_prompt")]
    fn verbatim_panics_in_debug_builds() {
        let _ = build_prompt("hi", Tone::Verbatim, &[], None);
    }
}
