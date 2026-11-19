// Persistent record of every line submitted through the unified input
// bar. tap owns this record — it is "what was typed into tap", not a
// shell history file: no HISTFILE parsing. The bar already sees every
// prompt-state keystroke, so its own record is effectively the full
// command history; the timestamp + cwd columns exist so future
// classification (per-directory / per-command ranking) needs no format
// migration. The optional fourth column is the command's exit status,
// spliced in by `backfill_exit` when the shell's OSC 133 `D` mark
// arrives — shells without integration never send it and their rows
// simply stay three columns.

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Both the rewrite threshold and the natural file size bound.
pub const MAX_ENTRIES: usize = 10_000;

const FILE_NAME: &str = "input-history";

/// One recorded submission. `line` is guaranteed single-line and
/// tab-free (the bar sanitizes `\n`/`\r`/`\t` to spaces), which is what
/// makes the TSV format safe without any escaping; `exit` is `None`
/// until the shell's OSC 133 `D` report lands (`backfill_exit`).
pub struct Entry {
    pub ts: u64,
    pub cwd: String,
    pub line: String,
    pub exit: Option<i32>,
}

/// `{config dir}/input-history`.
pub fn store_path() -> PathBuf {
    tap_backend::config::config_dir_path().join(FILE_NAME)
}

/// Seconds since the UNIX epoch; 0 if the clock is before it.
pub fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode(entry: &Entry) -> String {
    match entry.exit {
        Some(exit) => format!("{}\t{}\t{}\t{exit}\n", entry.ts, entry.cwd, entry.line),
        None => format!("{}\t{}\t{}\n", entry.ts, entry.cwd, entry.line),
    }
}

/// Parse one TSV record. Lines without the two separators are corrupt
/// (hand-edited file, older format) and skipped; anything after the
/// second tab is the line verbatim, except a trailing `\t<plain
/// integer>` which is the optional exit column — a stray tab inside a
/// hand-edited line survives unless its remainder happens to end in a
/// parseable integer.
fn parse(raw: &str) -> Option<Entry> {
    let mut parts = raw.splitn(3, '\t');
    let ts = parts.next()?.parse::<u64>().ok()?;
    let cwd = parts.next()?.to_string();
    let rest = parts.next()?.trim_end_matches('\n');
    let (line, exit) = match rest.rsplit_once('\t') {
        Some((head, tail)) if !tail.is_empty() => match tail.parse::<i32>() {
            Ok(exit) => (head, Some(exit)),
            Err(_) => (rest, None),
        },
        _ => (rest, None),
    };
    Some(Entry {
        ts,
        cwd,
        line: line.to_string(),
        exit,
    })
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

/// Per-pane bound on submissions awaiting their outcome. Generous
/// against bursts; shells without integration never receive a `D`, so
/// the cap keeps their queues flat.
pub const MAX_EXIT_PENDING: usize = 8;

/// Record one submission as awaiting the exit status of the command it
/// launches, keeping the queue bounded.
pub fn push_exit_candidate(queue: &mut VecDeque<(u64, String)>, ts: u64, line: &str) {
    queue.push_back((ts, line.to_string()));
    while queue.len() > MAX_EXIT_PENDING {
        queue.pop_front();
    }
}

/// Decide which recorded submission a finished command belongs to and
/// drain the queue accordingly. The shell executes submissions in
/// order, so a command that started at `started_at` belongs to the
/// NEWEST submission from on or before its start (a submit is never
/// younger than the command it launched, so no slack is needed; one
/// that arrived while the command was already running is younger than
/// the start and stays queued for the next `D`). Older in-window
/// submissions were consumed by the running program itself (lines fed
/// to `cat`, say) and are dropped without an exit. With no
/// `CommandStarted` in the event stream, the second-granularity `ts`
/// makes back-to-back same-second submits racy — the newest of them
/// wins and at most one row's exit column can be mislabeled.
pub fn exit_target(
    queue: &mut VecDeque<(u64, String)>,
    started_at: u64,
) -> Option<(u64, String)> {
    let mut target = None;
    let mut keep = VecDeque::with_capacity(queue.len());
    for item in queue.drain(..) {
        if item.0 <= started_at {
            target = Some(item);
        } else {
            keep.push_back(item);
        }
    }
    *queue = keep;
    target
}

/// Splice `exit` onto the newest store row recording `(ts, line)`
/// without an exit column yet. Every other byte of the file is
/// preserved verbatim — cwds and corrupt lines are never re-encoded —
/// so the only race is with a concurrent append from another window
/// (last writer wins, both valid).
pub fn backfill_exit(path: &Path, ts: u64, line: &str, exit: i32) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let mut rows: Vec<String> = content.lines().map(str::to_string).collect();
    let index = rows.iter().rposition(|row| {
        parse(row).is_some_and(|entry| {
            entry.ts == ts && entry.line == line && entry.exit.is_none()
        })
    });
    let Some(index) = index else {
        return;
    };
    rows[index].push_str(&format!("\t{exit}"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = rows.join("\n");
    body.push('\n');
    let _ = std::fs::write(path, body);
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
            exit: None,
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
            exit: None,
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
                exit: None,
            },
        );
        append(
            &store.0,
            &Entry {
                ts: 2,
                cwd: "/new".into(),
                line: "make".into(),
                exit: None,
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
                exit: None,
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

    #[test]
    fn exit_column_roundtrip() {
        let store = TempStore::new("exit-col");
        append(
            &store.0,
            &Entry {
                ts: 7,
                cwd: "/w".into(),
                line: "false".into(),
                exit: Some(1),
            },
        );
        append(&store.0, &entry("true"));

        let raw = std::fs::read_to_string(&store.0).unwrap();
        assert_eq!(raw, "7\t/w\tfalse\t1\n1700000000\t\ttrue\n");

        let entries = load_entries(&store.0, MAX_ENTRIES);
        assert_eq!(entries[0].exit, Some(1));
        assert_eq!(entries[1].exit, None);
    }

    #[test]
    fn numeric_stray_tab_tail_reads_as_exit() {
        // Documented edge: a hand-edited row whose remainder ends in a
        // parseable integer takes it as the exit column.
        let store = TempStore::new("tab-numeric");
        std::fs::write(&store.0, "1700000000\t/dir\techo a\t3\n").unwrap();
        let entries = load_entries(&store.0, MAX_ENTRIES);
        assert_eq!(entries[0].line, "echo a");
        assert_eq!(entries[0].exit, Some(3));
    }

    #[test]
    fn backfill_splices_the_newest_matching_row() {
        let store = TempStore::new("backfill");
        // Same second, same line re-run: the newest occurrence is the
        // command whose exit arrived.
        append(
            &store.0,
            &Entry {
                ts: 9,
                cwd: "/a".into(),
                line: "make".into(),
                exit: None,
            },
        );
        append(
            &store.0,
            &Entry {
                ts: 9,
                cwd: "/a".into(),
                line: "make".into(),
                exit: None,
            },
        );
        append(&store.0, &entry("ls"));

        backfill_exit(&store.0, 9, "make", 2);

        // Only the newest matching row is spliced; everything else is
        // byte-identical.
        assert_eq!(
            std::fs::read_to_string(&store.0).unwrap(),
            "9\t/a\tmake\n9\t/a\tmake\t2\n1700000000\t\tls\n"
        );
        // After dedup the surviving `make` row is the spliced one.
        let entries = load_entries(&store.0, MAX_ENTRIES);
        assert_eq!(entries[0].line, "make");
        assert_eq!(entries[0].exit, Some(2));
        assert_eq!(entries[1].line, "ls");
    }

    #[test]
    fn backfill_preserves_other_rows_verbatim() {
        let store = TempStore::new("backfill-raw");
        std::fs::write(
            &store.0,
            "not a tsv line\n5\t/keep\tcargo test\n6\t/patch\tgit st\n",
        )
        .unwrap();

        backfill_exit(&store.0, 6, "git st", 0);

        assert_eq!(
            std::fs::read_to_string(&store.0).unwrap(),
            "not a tsv line\n5\t/keep\tcargo test\n6\t/patch\tgit st\t0\n"
        );
    }

    #[test]
    fn backfill_is_idempotent_and_ignores_unknown_rows() {
        let store = TempStore::new("backfill-idem");
        append(
            &store.0,
            &Entry {
                ts: 3,
                cwd: String::new(),
                line: "ls".into(),
                exit: Some(0),
            },
        );
        let before = std::fs::read_to_string(&store.0).unwrap();

        backfill_exit(&store.0, 3, "ls", 1); // already has an exit
        backfill_exit(&store.0, 99, "nope", 1); // no such row

        assert_eq!(std::fs::read_to_string(&store.0).unwrap(), before);
    }

    #[test]
    fn exit_target_picks_the_newest_submission_before_the_start() {
        let mut queue = VecDeque::new();
        push_exit_candidate(&mut queue, 10, "ls");
        push_exit_candidate(&mut queue, 11, "cat data");

        // The command started at 10 (a submit from 11 arrived while it
        // was already running and stays queued).
        assert_eq!(exit_target(&mut queue, 10), Some((10, "ls".to_string())));
        assert_eq!(queue, VecDeque::from([(11, "cat data".to_string())]));
        assert_eq!(
            exit_target(&mut queue, 12),
            Some((11, "cat data".to_string()))
        );
        assert!(queue.is_empty());
        assert_eq!(exit_target(&mut queue, 13), None);
    }

    #[test]
    fn exit_target_drops_lines_consumed_by_the_running_program() {
        // Lines fed to a foreground `cat` never ran as commands: the
        // finished command's `D` drops them and backfills itself.
        let mut queue =
            VecDeque::from([(10, "cat data".to_string()), (12, "make".to_string())]);
        assert_eq!(exit_target(&mut queue, 12), Some((12, "make".to_string())));
        assert!(queue.is_empty());
    }

    #[test]
    fn push_exit_candidate_caps_the_queue() {
        let mut queue = VecDeque::new();
        for i in 0..(MAX_EXIT_PENDING + 4) {
            push_exit_candidate(&mut queue, i as u64, "cmd");
        }
        assert_eq!(queue.len(), MAX_EXIT_PENDING);
        assert_eq!(queue.front().map(|item| item.0), Some(4));
    }
}
