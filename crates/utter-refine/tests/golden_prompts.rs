//! Golden tests for `build_prompt`: compares the built system/user prompt
//! pair against a committed snapshot file for a handful of representative
//! (tone, dictionary, language hint) combinations.

use utter_core::Tone;
use utter_refine::build_prompt;

const CLEAN_NO_DICT_NO_LANG: &str = include_str!("golden/clean_no_dict_no_lang.snap.txt");
const FORMAL_DICT_LANG: &str = include_str!("golden/formal_dict_lang.snap.txt");
const NOTES_EMPTY_DICT_LANG: &str = include_str!("golden/notes_empty_dict_lang.snap.txt");
const CODE_COMMENT_DICT_NO_LANG: &str = include_str!("golden/code_comment_dict_no_lang.snap.txt");

/// Renders a `(system_prompt, user_content)` pair into the snapshot format:
/// the system prompt, then a `---USER---` delimiter line, then the user
/// content.
fn render_snapshot(system: &str, user: &str) -> String {
    format!("{system}\n---USER---\n{user}")
}

#[test]
fn clean_no_dict_no_lang() {
    let raw = "so um i think we should uh ship the the release tomorrow";
    let (system, user) = build_prompt(raw, Tone::Clean, &[], None);
    let actual = render_snapshot(&system, &user);
    assert_eq!(actual, CLEAN_NO_DICT_NO_LANG);
}

#[test]
fn formal_dict_lang() {
    let raw = "ну короче нам надо переделать этот отчет до пятницы";
    let terms = vec!["Postgres".to_string(), "Kubernetes".to_string()];
    let (system, user) = build_prompt(raw, Tone::Formal, &terms, Some("ru"));
    let actual = render_snapshot(&system, &user);
    assert_eq!(actual, FORMAL_DICT_LANG);
}

#[test]
fn notes_empty_dict_lang() {
    let raw = "okay so first thing check the budget then talk to sales then follow up friday";
    let (system, user) = build_prompt(raw, Tone::Notes, &[], Some("en"));
    let actual = render_snapshot(&system, &user);
    assert_eq!(actual, NOTES_EMPTY_DICT_LANG);
}

#[test]
fn code_comment_dict_no_lang() {
    let raw = "this function retries the sqlite write three times before giving up";
    let terms = vec!["SQLite".to_string()];
    let (system, user) = build_prompt(raw, Tone::CodeComment, &terms, None);
    let actual = render_snapshot(&system, &user);
    assert_eq!(actual, CODE_COMMENT_DICT_NO_LANG);
}
