//! Voice snippet matching: maps a spoken trigger phrase to a stored body.

/// A voice trigger → body mapping.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Snippet {
    /// The trigger phrase that, when normalized and matched against a transcript,
    /// causes the snippet to fire.
    pub trigger: String,
    /// The body text to insert when the trigger matches.
    pub body: String,
}

/// Normalize text for matching: lowercase via Unicode-aware case folding,
/// strip punctuation (keep only alphanumeric and whitespace), collapse whitespace runs
/// to single spaces, and trim leading/trailing whitespace.
pub fn normalize(text: &str) -> String {
    let normalized: String = text
        .chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c.to_lowercase().collect::<Vec<char>>()
            } else {
                vec![]
            }
        })
        .collect();

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Match the normalized transcript against the normalized triggers of each snippet.
///
/// Returns a reference to the first snippet whose normalized trigger equals the
/// normalized transcript. If the normalized trigger is empty, that snippet never matches.
/// Returns `None` if no snippet matches.
pub fn match_snippet<'a>(transcript: &str, snippets: &'a [Snippet]) -> Option<&'a Snippet> {
    let normalized_transcript = normalize(transcript);

    for snippet in snippets {
        let normalized_trigger = normalize(&snippet.trigger);
        if !normalized_trigger.is_empty() && normalized_trigger == normalized_transcript {
            return Some(snippet);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_and_strips_punctuation() {
        assert_eq!(
            normalize("Insert my email signature!"),
            "insert my email signature"
        );
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize("  Hello,   world "), "hello world");
    }

    #[test]
    fn normalize_preserves_alphanumeric_and_spaces() {
        assert_eq!(normalize("test123"), "test123");
    }

    #[test]
    fn normalize_handles_cyrillic() {
        // Cyrillic text should be lowercased and punctuation stripped
        assert_eq!(normalize("ПРИВЕТ, МИР!"), "привет мир");
    }

    #[test]
    fn snippet_exact_match_with_punctuation() {
        let snippets = vec![Snippet {
            trigger: "insert my email signature".to_string(),
            body: "signature body".to_string(),
        }];
        let matched = match_snippet("Insert my email signature!", &snippets);
        assert_eq!(matched, Some(&snippets[0]));
    }

    #[test]
    fn snippet_exact_match_case_insensitive() {
        let snippets = vec![Snippet {
            trigger: "insert my email signature".to_string(),
            body: "signature body".to_string(),
        }];
        let matched = match_snippet("INSERT MY EMAIL SIGNATURE", &snippets);
        assert_eq!(matched, Some(&snippets[0]));
    }

    #[test]
    fn snippet_partial_text_does_not_match() {
        let snippets = vec![Snippet {
            trigger: "insert my email signature".to_string(),
            body: "signature body".to_string(),
        }];
        let matched = match_snippet("insert my email", &snippets);
        assert_eq!(matched, None);
    }

    #[test]
    fn snippet_empty_list() {
        let snippets: Vec<Snippet> = vec![];
        let matched = match_snippet("insert my email signature", &snippets);
        assert_eq!(matched, None);
    }

    #[test]
    fn snippet_cyrillic_trigger() {
        let snippets = vec![Snippet {
            trigger: "привет мир".to_string(),
            body: "hello world".to_string(),
        }];
        let matched = match_snippet("Привет, Мир!", &snippets);
        assert_eq!(matched, Some(&snippets[0]));
    }

    #[test]
    fn snippet_first_matching_wins() {
        let snippets = vec![
            Snippet {
                trigger: "hello".to_string(),
                body: "first".to_string(),
            },
            Snippet {
                trigger: "hello".to_string(),
                body: "second".to_string(),
            },
        ];
        let matched = match_snippet("hello", &snippets);
        assert_eq!(matched, Some(&snippets[0]));
    }

    #[test]
    fn snippet_empty_trigger_never_matches() {
        let snippets = vec![Snippet {
            trigger: "".to_string(),
            body: "should not match".to_string(),
        }];
        let matched = match_snippet("", &snippets);
        assert_eq!(matched, None);
    }

    #[test]
    fn snippet_trigger_with_only_punctuation_never_matches() {
        let snippets = vec![Snippet {
            trigger: "!!!".to_string(),
            body: "should not match".to_string(),
        }];
        let matched = match_snippet("!!!", &snippets);
        assert_eq!(matched, None);
    }

    #[test]
    fn snippet_multiple_matches_first_wins() {
        let snippets = vec![
            Snippet {
                trigger: "foo bar".to_string(),
                body: "first body".to_string(),
            },
            Snippet {
                trigger: "baz qux".to_string(),
                body: "second body".to_string(),
            },
        ];
        let matched = match_snippet("foo bar", &snippets);
        assert_eq!(matched, Some(&snippets[0]));
    }
}
