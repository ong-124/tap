// Persistent record of every line submitted through the unified input
// bar. tap owns this record — it is "what was typed into tap", not a
// shell history file: no HISTFILE parsing, no shell integration. The
// bar already sees every prompt-state keystroke, so its own record is
// effectively the full command history; the timestamp + cwd columns
// exist so future classification (per-directory / per-command ranking)
// needs no format migration.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Both the rewrite threshold and the natural file size bound.
pub const MAX_ENTRIES: usize = 10_000;

const FILE_NAME: &str = "input-history";

/// One recorded submission. `line` is guaranteed single-line (the bar
/// sanitizes `\n`/`\r`/`\t` to spaces) and tab-free, which is what
/// makes the 3-column TSV format safe without any escaping.
pub struct Entry {
    pub ts: u64,
    pub cwd: String,
    pub line: String,
}

/// `{config dir}/input-history`.
pub fn store_path() -> PathBuf {
    rio_backend::config::config_dir_path().join(FILE_NAME)
}

/// Seconds since the UNIX epoch; 0 if the clock is before it.
pub fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode(entry: &Entry) -> String {
    format!("{}\t{}\t{}\n", entry.ts, entry.cwd, entry.line)
}

/// Parse one TSV record. Lines without the two separators are corrupt
/// (hand-edited file, older format) and skipped; anything after the
/// second tab is the line verbatim, so a stray tab inside it survives.
fn parse(raw: &str) -> Option<Entry> {
    let mut parts = raw.splitn(3, '\t');
    let ts = parts.next()?.parse::<u64>().ok()?;
    let cwd = parts.next()?.to_string();
    let line = parts.next()?.trim_end_matches('\n').to_string();
    Some(Entry { ts, cwd, line })
}

/// Load the recorded entries, oldest first. Duplicates collapse
/// globally keeping the *last* occurrence (`a b a` leaves `b`, `a` —
/// Warp dedups at display time the same way, history.rs:441-462), so
/// browsing never resurfaces a command that was re-run later; corrupt
/// lines are skipped, and when the store grew past `cap` it is
/// rewritten down to the newest `cap` entries. A missing or unreadable
/// file yields an empty list — the bar falls back to session-only
/// history.
pub fn load_entries(path: &Path, cap: usize) -> Vec<Entry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = content.lines().filter_map(parse).collect();

    let mut truncated = false;
    if entries.len() > cap {
        entries.drain(..entries.len() - cap);
        truncated = true;
    }

    // Full dedup keeping the newest occurrence, order stays oldest →
    // newest: walk backwards, keep first sightings, then reverse.
    let mut seen: HashSet<String> = HashSet::new();
    let mut deduped: Vec<Entry> = Vec::with_capacity(entries.len());
    for entry in entries.into_iter().rev() {
        if seen.insert(entry.line.clone()) {
            deduped.push(entry);
        }
    }
    deduped.reverse();

    if truncated {
        let lines: Vec<String> = deduped.iter().map(|e| e.line.clone()).collect();
        rewrite(path, &lines);
    }
    deduped
}

/// Append one submission. `Entry.cwd` comes from the caller (the
/// focused pane's cwd); this module does no syscalls beyond the file
/// append. The single `write_all` on an O_APPEND handle is atomic
/// enough for concurrent tap windows.
pub fn append(path: &Path, entry: &Entry) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    else {
        return;
    };
    let _ = file.write_all(encode(entry).as_bytes());
}

/// Rewrite the store with exactly these lines, oldest first. Used only
/// for the cap trim; last writer wins across windows, which is benign
/// since every writer stores a valid ≤cap tail.
fn rewrite(path: &Path, lines: &[String]) {
    let now = now_ts();
    let mut body = String::new();
    for line in lines {
        body.push_str(&encode(&Entry {
            ts: now,
            cwd: String::new(),
            line: line.clone(),
        }));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp file per test; cleaned up on drop so parallel tests
    /// never collide (the repo has no tempfile dev-dependency).
    struct TempStore(PathBuf);

    impl TempStore {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 =
                std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tap-input-history-{}-{n}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn entry(line: &str) -> Entry {
        Entry {
            ts: 1_700_000_000,
            cwd: String::new(),
            line: line.to_string(),
        }
    }

    fn lines(store: &TempStore) -> Vec<String> {
        load_entries(&store.0, MAX_ENTRIES)
            .into_iter()
            .map(|e| e.line)
            .collect()
    }

    #[test]
    fn append_and_load_roundtrip() {
        let store = TempStore::new("roundtrip");
        append(&store.0, &entry("ls -la"));
        append(&store.0, &entry("cargo test"));
        append(&store.0, &entry("echo 中文 ok"));

        assert_eq!(lines(&store), vec!["ls -la", "cargo test", "echo 中文 ok"]);
    }

    #[test]
    fn missing_file_is_empty() {
        let store = TempStore::new("missing");
        assert!(load_entries(&store.0, MAX_ENTRIES).is_empty());
    }

    #[test]
    fn duplicates_collapse_globally_keeping_the_latest() {
        let store = TempStore::new("dedup");
        append(&store.0, &entry("git status"));
        append(&store.0, &entry("git status"));
        append(&store.0, &entry("ls"));
        append(&store.0, &entry("git status"));

        // `a b a` leaves two entries ordered by their last occurrence,
        // so ↑ browsing sees `git status` exactly once, as the newest.
        assert_eq!(lines(&store), vec!["ls", "git status"]);
    }

    #[test]
    fn dedup_keeps_the_last_occurrences_cwd() {
        let store = TempStore::new("dedup-cwd");
        append(
            &store.0,
            &Entry {
                ts: 1,
                cwd: "/old".into(),
                line: "make".into(),
            },
        );
        append(
            &store.0,
            &Entry {
                ts: 2,
                cwd: "/new".into(),
                line: "make".into(),
            },
        );

        let entries = load_entries(&store.0, MAX_ENTRIES);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].cwd, "/new");
    }

    #[test]
    fn cap_rewrites_file_to_tail() {
        let store = TempStore::new("cap");
        for i in 0..8 {
            append(&store.0, &entry(&format!("cmd-{i}")));
        }
        let tail = vec!["cmd-4", "cmd-5", "cmd-6", "cmd-7"];
        let loaded: Vec<String> = load_entries(&store.0, 4)
            .into_iter()
            .map(|e| e.line)
            .collect();
        assert_eq!(loaded, tail);

        // The rewrite persisted: a second load sees the same trimmed
        // tail, and further appends keep working on top of it.
        assert_eq!(lines(&store), tail);
        append(&store.0, &entry("after"));
        let mut expected = tail;
        expected.push("after");
        assert_eq!(lines(&store), expected);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let store = TempStore::new("corrupt");
        std::fs::write(
            &store.0,
            "not a tsv line\n1700000000\tonly-two-columns\n1700000000\t/home/u\tls -la\nnots\t\n",
        )
        .unwrap();

        assert_eq!(lines(&store), vec!["ls -la"]);
    }

    #[test]
    fn empty_cwd_survives_the_roundtrip() {
        let store = TempStore::new("empty-cwd");
        append(
            &store.0,
            &Entry {
                ts: 42,
                cwd: String::new(),
                line: "pwd".to_string(),
            },
        );

        let raw = std::fs::read_to_string(&store.0).unwrap();
        assert_eq!(raw, "42\t\tpwd\n");
        assert_eq!(lines(&store), vec!["pwd"]);
    }

    #[test]
    fn stray_tab_inside_line_stays_verbatim() {
        // Hand-edited rows can still hold a third tab; parse keeps the
        // remainder as the line instead of dropping it.
        let store = TempStore::new("tab-in-line");
        std::fs::write(&store.0, "1700000000\t/dir\techo a\tb\n").unwrap();
        assert_eq!(lines(&store), vec!["echo a\tb"]);
    }
}
