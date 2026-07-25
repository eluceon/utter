//! Dictionary replacement rules: whole-word, case-insensitive, literal substitution
//! applied to a transcript before any LLM refinement.

/// A single "heard X, write Y" dictionary entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplaceRule {
    /// The (mis-heard) phrase to look for, matched whole-word and case-insensitively.
    pub heard: String,
    /// The literal replacement text, inserted with its casing preserved.
    pub write: String,
}

/// A rule's `heard` phrase normalized for matching: inner whitespace collapsed
/// to single spaces, ready for case-insensitive character comparison.
struct CompiledRule<'a> {
    heard_chars: Vec<char>,
    write: &'a str,
}

fn compile_rules(rules: &[ReplaceRule]) -> Vec<CompiledRule<'_>> {
    rules
        .iter()
        .filter(|r| !r.heard.trim().is_empty())
        .map(|r| CompiledRule {
            heard_chars: r
                .heard
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .collect(),
            write: r.write.as_str(),
        })
        .collect()
}

/// Returns whether `text` starting at char index `pos` (given as the full char
/// vector) matches `pattern` case-insensitively, and whether both boundaries
/// (before `pos` and after the match) fall on word boundaries.
fn match_at(text_chars: &[char], pos: usize, pattern: &[char]) -> bool {
    if pattern.is_empty() || pos + pattern.len() > text_chars.len() {
        return false;
    }

    // Left boundary: start of string, or previous char is not alphanumeric.
    if pos > 0 && text_chars[pos - 1].is_alphanumeric() {
        return false;
    }

    // Character-by-character case-insensitive comparison.
    for (offset, &pat_ch) in pattern.iter().enumerate() {
        let text_ch = text_chars[pos + offset];
        if !chars_eq_ci(text_ch, pat_ch) {
            return false;
        }
    }

    // Right boundary: end of string, or next char is not alphanumeric.
    let end = pos + pattern.len();
    if end < text_chars.len() && text_chars[end].is_alphanumeric() {
        return false;
    }

    true
}

/// Compares two characters for equality using full-Unicode case folding.
fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Applies dictionary replacement rules to `text`.
///
/// Matching is whole-word and case-insensitive; the `write` replacement is
/// inserted verbatim, casing preserved. Rules are tried in declaration order
/// at each scan position, so the first matching rule wins on overlap. Once a
/// rule matches, the scan resumes right after the matched span (no
/// re-matching inside a replacement). Rules with an empty (or all-whitespace)
/// `heard` phrase never match.
pub fn apply_rules(text: &str, rules: &[ReplaceRule]) -> String {
    let compiled = compile_rules(rules);
    if compiled.is_empty() {
        return text.to_string();
    }

    let text_chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while pos < text_chars.len() {
        let matched_rule = compiled
            .iter()
            .find(|r| match_at(&text_chars, pos, &r.heard_chars));

        match matched_rule {
            Some(r) => {
                result.push_str(r.write);
                pos += r.heard_chars.len();
            }
            None => {
                result.push(text_chars[pos]);
                pos += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(heard: &str, write: &str) -> ReplaceRule {
        ReplaceRule {
            heard: heard.to_string(),
            write: write.to_string(),
        }
    }

    #[test]
    fn whole_word_match() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("rust sqlite", &rules), "rust SQLite");
    }

    #[test]
    fn case_insensitive_match() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("rust SQLITE", &rules), "rust SQLite");
    }

    #[test]
    fn no_substring_hits() {
        let rules = vec![rule("script", "SCRIPT")];
        assert_eq!(apply_rules("transcription", &rules), "transcription");
    }

    #[test]
    fn multi_word_heard_phrase() {
        let rules = vec![rule("my sequel", "MySQL")];
        assert_eq!(
            apply_rules("i love my sequel database", &rules),
            "i love MySQL database"
        );
    }

    #[test]
    fn unicode_cyrillic_input() {
        let rules = vec![rule("питон", "Python")];
        assert_eq!(
            apply_rules("я пишу на питоне и питон", &rules),
            "я пишу на питоне и Python"
        );
    }

    #[test]
    fn empty_rules_unchanged() {
        let rules: Vec<ReplaceRule> = vec![];
        assert_eq!(apply_rules("rust sqlite", &rules), "rust sqlite");
    }

    #[test]
    fn preserves_surrounding_punctuation() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("use sqlite.", &rules), "use SQLite.");
    }

    #[test]
    fn rule_at_start_of_string() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("sqlite is great", &rules), "SQLite is great");
    }

    #[test]
    fn rule_at_end_of_string() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("i use sqlite", &rules), "i use SQLite");
    }

    #[test]
    fn adjacent_punctuation() {
        let rules = vec![rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("(sqlite)", &rules), "(SQLite)");
    }

    #[test]
    fn overlapping_rules_first_declared_wins() {
        // "sqlite database" would match both rules; the first-declared rule
        // should win and the second should never get a chance to fire.
        let rules = vec![
            rule("sqlite database", "SQLite DB"),
            rule("sqlite", "SQLite"),
        ];
        assert_eq!(
            apply_rules("i use sqlite database daily", &rules),
            "i use SQLite DB daily"
        );
    }

    #[test]
    fn heard_differing_only_in_case_from_another_rule() {
        // Two rules whose `heard` values are the same word modulo case: since
        // matching is case-insensitive, the first-declared rule always wins.
        let rules = vec![rule("SQLite", "first-wins"), rule("sqlite", "second")];
        assert_eq!(apply_rules("i use sqlite", &rules), "i use first-wins");
    }

    #[test]
    fn empty_heard_string_never_matches() {
        let rules = vec![rule("", "SHOULD-NOT-APPEAR"), rule("sqlite", "SQLite")];
        assert_eq!(apply_rules("use sqlite", &rules), "use SQLite");
    }
}
