//! SQLite-backed dictation history repository.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, Row};
use serde::Serialize;

/// The current on-disk schema version, tracked via SQLite's `user_version` pragma.
const SCHEMA_VERSION: i64 = 1;

/// A single stored dictation history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: i64,
    pub duration_ms: i64,
    pub engine: String,
    pub raw_text: String,
    pub final_text: String,
    pub app: Option<String>,
}

/// Fields required to record a new history entry.
///
/// `created_at` is not part of this struct: `HistoryRepo::add` stamps it with
/// the current unix time when the entry is inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    pub duration_ms: i64,
    pub engine: String,
    pub raw_text: String,
    pub final_text: String,
    pub app: Option<String>,
}

/// A SQLite-backed store of completed dictation history.
pub struct HistoryRepo {
    conn: Connection,
}

impl HistoryRepo {
    /// Opens (creating if necessary) the history database at `path`, creating
    /// parent directories as needed, and runs any pending schema migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create parent directories")?;
        }
        let conn = Connection::open(path).context("failed to open database")?;
        migrate(&conn)?;
        Ok(HistoryRepo { conn })
    }

    /// Inserts a new history entry, stamping `created_at` with the current
    /// unix time, and returns the id of the inserted row.
    pub fn add(&self, e: NewEntry) -> Result<i64> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("failed to get current time")?
            .as_secs() as i64;

        self.conn
            .execute(
                "INSERT INTO history (created_at, duration_ms, engine, raw_text, final_text, app)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    created_at,
                    e.duration_ms,
                    e.engine,
                    e.raw_text,
                    e.final_text,
                    e.app
                ],
            )
            .context("failed to insert history entry")?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Lists history entries newest-first, optionally filtered to those whose
    /// `final_text` contains `query` as a substring.
    ///
    /// Note: the substring match uses SQLite's default `LIKE`, which is only
    /// case-insensitive for ASCII letters; non-ASCII characters are matched
    /// case-sensitively.
    pub fn list(&self, query: Option<&str>, limit: u32) -> Result<Vec<HistoryEntry>> {
        let query_filter = query.and_then(|q| {
            let trimmed = q.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(escape_like(trimmed))
            }
        });

        let mut entries = Vec::new();

        if let Some(escaped_query) = query_filter {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, created_at, duration_ms, engine, raw_text, final_text, app
                 FROM history
                 WHERE final_text LIKE '%' || ?1 || '%' ESCAPE '\\'
                 ORDER BY id DESC
                 LIMIT ?2",
                )
                .context("failed to prepare select with filter")?;

            let rows = stmt
                .query_map(params![escaped_query, limit as i64], row_to_entry)
                .context("failed to query history")?;

            for row in rows {
                entries.push(row.context("failed to map row to entry")?);
            }
        } else {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, created_at, duration_ms, engine, raw_text, final_text, app
                 FROM history
                 ORDER BY id DESC
                 LIMIT ?1",
                )
                .context("failed to prepare select without filter")?;

            let rows = stmt
                .query_map(params![limit as i64], row_to_entry)
                .context("failed to query history")?;

            for row in rows {
                entries.push(row.context("failed to map row to entry")?);
            }
        }

        Ok(entries)
    }

    /// Deletes the history entry with the given id, if it exists.
    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE id = ?1", params![id])
            .context("failed to delete history entry")?;
        Ok(())
    }

    /// Deletes all history entries.
    pub fn clear(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM history", [])
            .context("failed to clear history")?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("failed to read user_version")?;

    if user_version < SCHEMA_VERSION {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                engine TEXT NOT NULL,
                raw_text TEXT NOT NULL,
                final_text TEXT NOT NULL,
                app TEXT
            );
            PRAGMA user_version = 1;",
        )
        .context("failed to create history table or set user_version")?;
    }

    Ok(())
}

fn row_to_entry(row: &Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        duration_ms: row.get(2)?,
        engine: row.get(3)?,
        raw_text: row.get(4)?,
        final_text: row.get(5)?,
        app: row.get(6)?,
    })
}

/// Escapes `%`, `_`, and `\` in `q` so it can be embedded verbatim inside a
/// `LIKE ... ESCAPE '\'` pattern without its characters being treated as
/// wildcards.
fn escape_like(q: &str) -> String {
    let mut result = String::with_capacity(q.len());
    for c in q.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '%' => result.push_str("\\%"),
            '_' => result.push_str("\\_"),
            other => result.push(other),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("history.sqlite3")
    }

    fn entry(final_text: &str) -> NewEntry {
        NewEntry {
            duration_ms: 1234,
            engine: "whisper".to_string(),
            raw_text: format!("raw: {final_text}"),
            final_text: final_text.to_string(),
            app: Some("Editor".to_string()),
        }
    }

    #[test]
    fn add_and_list_roundtrip_orders_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        repo.add(entry("first")).expect("add first");
        repo.add(entry("second")).expect("add second");
        repo.add(entry("third")).expect("add third");

        let listed = repo.list(None, 10).expect("list");
        let texts: Vec<&str> = listed.iter().map(|e| e.final_text.as_str()).collect();

        assert_eq!(texts, vec!["third", "second", "first"]);
    }

    #[test]
    fn added_entry_fields_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        let id = repo.add(entry("hello world")).expect("add");
        let listed = repo.list(None, 10).expect("list");

        assert_eq!(listed.len(), 1);
        let got = &listed[0];
        assert_eq!(got.id, id);
        assert_eq!(got.duration_ms, 1234);
        assert_eq!(got.engine, "whisper");
        assert_eq!(got.raw_text, "raw: hello world");
        assert_eq!(got.final_text, "hello world");
        assert_eq!(got.app.as_deref(), Some("Editor"));
        assert!(got.created_at > 0);
    }

    #[test]
    fn search_filter_matches_middle_row_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        repo.add(entry("buy milk")).expect("add");
        repo.add(entry("call the plumber")).expect("add");
        repo.add(entry("finish the report")).expect("add");

        let listed = repo.list(Some("plumber"), 10).expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].final_text, "call the plumber");
    }

    #[test]
    fn empty_or_whitespace_query_applies_no_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        repo.add(entry("alpha")).expect("add");
        repo.add(entry("beta")).expect("add");

        let empty = repo.list(Some(""), 10).expect("list empty");
        let whitespace = repo.list(Some("   "), 10).expect("list whitespace");

        assert_eq!(empty.len(), 2);
        assert_eq!(whitespace.len(), 2);
    }

    #[test]
    fn search_with_percent_literal_does_not_wildcard_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        // Contains the literal substring "50%".
        repo.add(entry("50% off today")).expect("add");
        // Contains "50" but no literal "%"; an unescaped LIKE pattern for the
        // query "50%" would treat the trailing "%" as a wildcard and match
        // this row too, since it starts with "50" followed by more text.
        repo.add(entry("500000 widgets in stock")).expect("add");

        let listed = repo.list(Some("50%"), 10).expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].final_text, "50% off today");
    }

    #[test]
    fn delete_removes_one_leaves_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        repo.add(entry("keep one")).expect("add");
        let doomed_id = repo.add(entry("delete me")).expect("add");
        repo.add(entry("keep two")).expect("add");

        repo.delete(doomed_id).expect("delete");
        let listed = repo.list(None, 10).expect("list");
        let texts: Vec<&str> = listed.iter().map(|e| e.final_text.as_str()).collect();

        assert_eq!(texts, vec!["keep two", "keep one"]);
    }

    #[test]
    fn clear_empties_all_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        repo.add(entry("one")).expect("add");
        repo.add(entry("two")).expect("add");
        repo.add(entry("three")).expect("add");

        repo.clear().expect("clear");
        let listed = repo.list(None, 10).expect("list");

        assert!(listed.is_empty());
    }

    #[test]
    fn reopening_the_same_path_keeps_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = repo_path(&dir);

        {
            let repo = HistoryRepo::open(&path).expect("open");
            repo.add(entry("persisted")).expect("add");
        }

        let repo = HistoryRepo::open(&path).expect("reopen");
        let listed = repo.list(None, 10).expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].final_text, "persisted");
    }

    #[test]
    fn migration_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = repo_path(&dir);

        {
            let repo = HistoryRepo::open(&path).expect("open");
            drop(repo);
        }
        {
            let repo = HistoryRepo::open(&path).expect("reopen should not error");
            drop(repo);
        }

        let conn = Connection::open(&path).expect("open raw connection");
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read user_version");

        assert_eq!(user_version, SCHEMA_VERSION);
    }

    #[test]
    fn limit_is_respected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = HistoryRepo::open(&repo_path(&dir)).expect("open");

        for i in 0..5 {
            repo.add(entry(&format!("entry {i}"))).expect("add");
        }

        let listed = repo.list(None, 2).expect("list");
        let texts: Vec<&str> = listed.iter().map(|e| e.final_text.as_str()).collect();

        assert_eq!(texts, vec!["entry 4", "entry 3"]);
    }

    #[test]
    fn escape_like_escapes_percent() {
        assert_eq!(escape_like("50%"), "50\\%");
    }

    #[test]
    fn escape_like_escapes_underscore() {
        assert_eq!(escape_like("a_b"), "a\\_b");
    }

    #[test]
    fn escape_like_escapes_backslash() {
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_like_leaves_plain_text_unchanged() {
        assert_eq!(escape_like("plain text"), "plain text");
    }
}
