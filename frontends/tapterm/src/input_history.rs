// Persistent record of every line submitted through the unified input
// bar, stored in a SQLite database (`{config dir}/input-history.db`).
// tap owns this record — it is "what was typed into tap", not a shell
// history file: no HISTFILE parsing.
//
// The store is a three-table model: `dirs` holds each scope's
// directory once, `commands` each distinct line once with a lifetime
// use counter, and `usages` links the two (which directory ran which
// command, how often, how recently). Dedup happens in every table —
// a path never records twice, a line never records twice, a pair
// never links twice — so the browse semantics are keyed by scope:
// local panes scope by cwd, ssh sessions by (canonical host, remote
// cwd), and ↑ in a directory resurfaces exactly what ran there.
//
// The Connection lives in a process-wide mutex: one writer, WAL on,
// every write a short transaction — concurrent windows merge without
// read-modify-write races (the TSV store's fatal flaw). The optional
// exit status is spliced by `backfill_exit` when the shell's OSC 133
// `D` mark arrives, anchored on the usages row id `record` returned —
// no timestamp guessing. A corrupt database file is removed and
// recreated rather than poisoning every session.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex as ParkMutex;
use rusqlite::{params, Connection};

/// The store trim bound, run once per process at first use. Also the
/// in-memory browse bound in the bar.
pub const MAX_ENTRIES: usize = 10_000;

/// How many lines the no-scope fallback pool carries (uses desc).
pub const MAX_FREQUENT: usize = 255;

const FILE_NAME: &str = "input-history.db";

/// One recorded scope×command pair: where it ran, the line, the most
/// recent run there and its exit. `host` empty = local.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub host: String,
    pub cwd: String,
    pub line: String,
    pub ts: u64,
    pub exit: Option<i32>,
}

/// What startup loads into the bar: the browse entries plus the
/// global frequency pool backing the no-scope fallback.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub frequent: Vec<String>,
}

/// `{config dir}/input-history.db`.
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

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS dirs (
    id   INTEGER PRIMARY KEY,
    host TEXT NOT NULL DEFAULT '',
    cwd  TEXT NOT NULL DEFAULT '',
    UNIQUE (host, cwd)
);
CREATE TABLE IF NOT EXISTS commands (
    id   INTEGER PRIMARY KEY,
    line TEXT NOT NULL UNIQUE,
    uses INTEGER NOT NULL DEFAULT 1,
    ts   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usages (
    id         INTEGER PRIMARY KEY,
    dir_id     INTEGER NOT NULL REFERENCES dirs(id),
    command_id INTEGER NOT NULL REFERENCES commands(id),
    ts         INTEGER NOT NULL,
    uses       INTEGER NOT NULL DEFAULT 1,
    exit       INTEGER,
    UNIQUE (dir_id, command_id)
);";

fn open_conn(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 500)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Open the store, resurrecting it if the file is corrupt — a broken
/// database must not poison every future session the way a half-TSV
/// line once merely got skipped.
fn open_store() -> Option<Connection> {
    match open_conn(&store_path()) {
        Ok(conn) => Some(conn),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::DatabaseCorrupt =>
        {
            let _ = std::fs::remove_file(&store_path());
            let _ = std::fs::remove_file(store_path().with_extension("db-wal"));
            let _ = std::fs::remove_file(store_path().with_extension("db-shm"));
            open_conn(&store_path()).ok()
        }
        Err(_) => None,
    }
}

/// Run `f` against the process-wide connection (opened on first use,
/// trimmed to the cap then). Any store failure degrades to no-op: the
/// bar falls back to session-only history.
fn with_conn<T>(f: impl FnOnce(&mut Connection) -> T) -> Option<T> {
    static CONN: OnceLock<Option<ParkMutex<Connection>>> = OnceLock::new();
    let cell = CONN.get_or_init(|| {
        open_store().map(|mut conn| {
            trim_on(&mut conn, MAX_ENTRIES);
            ParkMutex::new(conn)
        })
    });
    let conn = cell.as_ref()?;
    Some(f(&mut conn.lock()))
}

/// Record one submission under its scope and return the usages row id
/// the eventual OSC 133 `D` exit belongs to. The three upserts run in
/// one transaction: the scope's dir row and the command row each exist
/// exactly once (their tables dedup), the pair links once, a re-run
/// just bumps the counters and clears the stale exit.
pub fn record(host: &str, cwd: &str, line: &str, ts: u64) -> Option<i64> {
    with_conn(|conn| record_on(conn, host, cwd, line, ts)).flatten()
}

fn record_on(
    conn: &mut Connection,
    host: &str,
    cwd: &str,
    line: &str,
    ts: u64,
) -> Option<i64> {
    let tx = conn.transaction().ok()?;
    tx.execute(
        "INSERT INTO dirs (host, cwd) VALUES (?1, ?2)
         ON CONFLICT (host, cwd) DO NOTHING",
        params![host, cwd],
    )
    .ok()?;
    let dir_id: i64 = tx
        .query_row(
            "SELECT id FROM dirs WHERE host = ?1 AND cwd = ?2",
            params![host, cwd],
            |row| row.get(0),
        )
        .ok()?;
    tx.execute(
        "INSERT INTO commands (line, uses, ts) VALUES (?1, 1, ?2)
         ON CONFLICT (line) DO UPDATE SET uses = uses + 1, ts = ?2",
        params![line, ts as i64],
    )
    .ok()?;
    let command_id: i64 = tx
        .query_row(
            "SELECT id FROM commands WHERE line = ?1",
            params![line],
            |row| row.get(0),
        )
        .ok()?;
    let usage_id: i64 = tx
        .query_row(
            "INSERT INTO usages (dir_id, command_id, ts, uses) VALUES (?1, ?2, ?3, 1)
             ON CONFLICT (dir_id, command_id)
             DO UPDATE SET ts = ?3, uses = uses + 1, exit = NULL
             RETURNING id",
            params![dir_id, command_id, ts as i64],
            |row| row.get(0),
        )
        .ok()?;
    tx.commit().ok()?;
    Some(usage_id)
}

/// Load the browse snapshot: one entry per scope×command pair, oldest
/// use first (the bar reverses per scope for its newest-first walk),
/// plus the global frequency pool for the no-scope fallback.
pub fn load() -> Snapshot {
    with_conn(|conn| Snapshot {
        entries: load_entries_on(conn, MAX_ENTRIES),
        frequent: frequent_on(conn, MAX_FREQUENT),
    })
    .unwrap_or_default()
}

fn load_entries_on(conn: &Connection, cap: usize) -> Vec<Entry> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT d.host, d.cwd, c.line, u.ts, u.exit
         FROM usages u
         JOIN dirs d ON d.id = u.dir_id
         JOIN commands c ON c.id = u.command_id
         ORDER BY u.ts ASC, u.id ASC
         LIMIT ?1",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![cap as i64], |row| {
        Ok(Entry {
            host: row.get(0)?,
            cwd: row.get(1)?,
            line: row.get(2)?,
            ts: row.get::<_, i64>(3)?.max(0) as u64,
            exit: row.get(4)?,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(|row| row.ok()).collect()
}

fn frequent_on(conn: &Connection, limit: usize) -> Vec<String> {
    let Ok(mut stmt) =
        conn.prepare("SELECT line FROM commands ORDER BY uses DESC, ts DESC LIMIT ?1")
    else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![limit as i64], |row| row.get(0)) else {
        return Vec::new();
    };
    rows.filter_map(|row| row.ok()).collect()
}

/// Drop usages beyond the newest `cap` and the dirs they orphaned.
/// `commands` rows survive on purpose: their lifetime `uses` is the
/// fallback ranking's data, and the set of distinct lines ever used is
/// naturally bounded. One transaction, once per process.
fn trim_on(conn: &mut Connection, cap: usize) {
    let Ok(tx) = conn.transaction() else {
        return;
    };
    let _ = tx.execute(
        "DELETE FROM usages WHERE id NOT IN (
            SELECT id FROM usages ORDER BY ts DESC, id DESC LIMIT ?1)",
        params![cap as i64],
    );
    let _ = tx.execute(
        "DELETE FROM dirs WHERE id NOT IN (SELECT DISTINCT dir_id FROM usages)",
        [],
    );
    let _ = tx.commit();
}

/// Splice the exit status onto the exact usages row `record` handed
/// back — one UPDATE by primary key, no scan, no race beyond SQLite's
/// own locking.
pub fn backfill_exit(usage_id: i64, exit: i32) {
    with_conn(|conn| backfill_on(conn, usage_id, exit));
}

fn backfill_on(conn: &mut Connection, usage_id: i64, exit: i32) {
    let _ = conn.execute(
        "UPDATE usages SET exit = ?1 WHERE id = ?2",
        params![exit, usage_id],
    );
}

/// Per-pane bound on submissions awaiting their outcome. Generous
/// against bursts; shells without integration never receive a `D`, so
/// the cap keeps their queues flat.
pub const MAX_EXIT_PENDING: usize = 8;

/// A submission awaiting its exit: when it was submitted and the
/// usages row the outcome belongs to.
pub type ExitCandidate = (u64, i64);

/// Record one submission as awaiting the exit status of the command it
/// launches, keeping the queue bounded.
pub fn push_exit_candidate(queue: &mut VecDeque<ExitCandidate>, ts: u64, usage_id: i64) {
    queue.push_back((ts, usage_id));
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
pub fn exit_target(queue: &mut VecDeque<ExitCandidate>, started_at: u64) -> Option<i64> {
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
    target.map(|(_, usage_id)| usage_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(SCHEMA).expect("schema");
        trim_on(&mut conn, MAX_ENTRIES);
        conn
    }

    fn record(conn: &mut Connection, host: &str, cwd: &str, line: &str, ts: u64) -> i64 {
        record_on(conn, host, cwd, line, ts).expect("record")
    }

    fn entries(conn: &Connection) -> Vec<Entry> {
        load_entries_on(conn, MAX_ENTRIES)
    }

    fn usage_row(conn: &Connection, id: i64) -> (u64, u64, Option<i32>) {
        conn.query_row(
            "SELECT ts, uses, exit FROM usages WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get(2)?,
                ))
            },
        )
        .expect("usage row")
    }

    fn uses_count(conn: &Connection, line: &str) -> u64 {
        conn.query_row(
            "SELECT uses FROM commands WHERE line = ?1",
            params![line],
            |row| row.get::<_, i64>(0).map(|v| v.max(0) as u64),
        )
        .expect("command row")
    }

    #[test]
    fn scope_and_command_each_record_once() {
        let mut conn = conn();
        record(&mut conn, "", "/a", "make", 1);
        record(&mut conn, "", "/a", "make", 2);
        record(&mut conn, "", "/b", "make", 3);
        record(&mut conn, "web-1", "/srv", "make", 4);

        let counts: i64 = conn
            .query_row("SELECT COUNT(*) FROM dirs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(counts, 3, "local /a, local /b, web-1 /srv");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "one distinct line");

        let usage_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count, 3, "one pair per scope");

        assert_eq!(uses_count(&conn, "make"), 4, "lifetime counter");
    }

    #[test]
    fn rerun_refreshes_ts_and_clears_the_stale_exit() {
        let mut conn = conn();
        let id = record(&mut conn, "", "/a", "make", 1);
        backfill_on(&mut conn, id, 2);
        assert_eq!(usage_row(&conn, id), (1, 1, Some(2)));

        let id2 = record(&mut conn, "", "/a", "make", 5);
        assert_eq!(id2, id, "the pair links to the same row");
        assert_eq!(
            usage_row(&conn, id),
            (5, 2, None),
            "ts bumped, exit cleared"
        );
    }

    #[test]
    fn load_joins_scope_and_orders_oldest_first() {
        let mut conn = conn();
        record(&mut conn, "", "/a", "first", 1);
        record(&mut conn, "web-1", "/srv", "second", 2);
        record(&mut conn, "", "/a", "third", 3);

        let loaded = entries(&conn);
        assert_eq!(loaded.len(), 3);
        assert_eq!(
            loaded[0],
            Entry {
                host: String::new(),
                cwd: "/a".into(),
                line: "first".into(),
                ts: 1,
                exit: None,
            }
        );
        assert_eq!(loaded[1].host, "web-1");
        assert_eq!(loaded[1].cwd, "/srv");
        assert_eq!(loaded[2].line, "third");
    }

    #[test]
    fn backfill_lands_on_the_exact_recorded_row() {
        let mut conn = conn();
        let a = record(&mut conn, "", "/a", "make", 1);
        let b = record(&mut conn, "", "/b", "make", 2);

        backfill_on(&mut conn, a, 0);
        assert_eq!(usage_row(&conn, a).2, Some(0));
        assert_eq!(usage_row(&conn, b).2, None, "the sibling row untouched");

        // Idempotent overwrite by id; unknown ids are no-ops.
        backfill_on(&mut conn, a, 3);
        assert_eq!(usage_row(&conn, a).2, Some(3));
        backfill_on(&mut conn, 99, 1);
    }

    #[test]
    fn frequent_pool_ranks_by_lifetime_uses() {
        let mut conn = conn();
        for i in 0..3 {
            record(&mut conn, "", "/a", "git status", i);
        }
        record(&mut conn, "", "/b", "git status", 3);
        record(&mut conn, "", "/b", "ls", 4);

        let pool = frequent_on(&conn, MAX_FREQUENT);
        assert_eq!(pool, vec!["git status", "ls"]);

        let capped = frequent_on(&conn, 1);
        assert_eq!(capped, vec!["git status"]);
    }

    #[test]
    fn trim_keeps_the_newest_cap_and_drops_orphan_dirs() {
        let mut conn = conn();
        record(&mut conn, "", "/old", "ancient", 1);
        record(&mut conn, "", "/mid", "older", 2);
        record(&mut conn, "", "/new", "recent", 3);

        trim_on(&mut conn, 2);

        let lines: Vec<String> = entries(&conn).into_iter().map(|e| e.line).collect();
        assert_eq!(lines, vec!["older", "recent"], "oldest usage trimmed");

        let dirs: i64 = conn
            .query_row("SELECT COUNT(*) FROM dirs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(dirs, 2, "/old orphaned, dropped");

        // The trimmed command still counts its lifetime uses.
        assert_eq!(uses_count(&conn, "ancient"), 1);
    }

    #[test]
    fn open_conn_is_idempotent() {
        let dir = std::env::temp_dir()
            .join(format!("tap-history-{}-schema", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let _ = std::fs::remove_file(&path);

        let _ = open_conn(&path).expect("first open");
        let _ = open_conn(&path).expect("second open creates no conflict");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exit_target_picks_the_newest_submission_before_the_start() {
        let mut queue = VecDeque::new();
        push_exit_candidate(&mut queue, 10, 101);
        push_exit_candidate(&mut queue, 11, 102);

        // The command started at 10 (a submit from 11 arrived while it
        // was already running and stays queued).
        assert_eq!(exit_target(&mut queue, 10), Some(101));
        assert_eq!(queue, VecDeque::from([(11, 102)]));
        assert_eq!(exit_target(&mut queue, 12), Some(102));
        assert!(queue.is_empty());
        assert_eq!(exit_target(&mut queue, 13), None);
    }

    #[test]
    fn exit_target_drops_lines_consumed_by_the_running_program() {
        // Lines fed to a foreground `cat` never ran as commands: the
        // finished command's `D` drops them and backfills itself.
        let mut queue = VecDeque::from([(10, 201), (12, 202)]);
        assert_eq!(exit_target(&mut queue, 12), Some(202));
        assert!(queue.is_empty());
    }

    #[test]
    fn push_exit_candidate_caps_the_queue() {
        let mut queue = VecDeque::new();
        for i in 0..(MAX_EXIT_PENDING + 4) {
            push_exit_candidate(&mut queue, i as u64, i as i64);
        }
        assert_eq!(queue.len(), MAX_EXIT_PENDING);
        assert_eq!(queue.front().map(|item| item.0), Some(4));
    }
}
