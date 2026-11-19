// Tab-completion candidate gathering for the unified input bar. Pure
// functions over an injected environment — no process-global state —
// so tests build `Sources` by hand instead of touching `PATH`.
//
// Two modes: the first word of the line completes against commands
// (PATH executables + a builtin table); any other word completes
// filesystem paths anchored at the focused pane's cwd.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Hard cap on returned candidates; keeps a bare-word Tab at the root
/// of a huge directory from flooding the candidates row.
const MAX_CANDIDATES: usize = 100;

/// Everything `gather` needs, resolved once per Tab press by the
/// caller (screen layer): PATH for command mode, home for `~`
/// expansion, and the focused pane's cwd for relative paths.
/// `session_ssh_host` is the destination of the ssh session running in
/// the focused pane (from `/proc` probing), `None` on a local pane.
pub struct Sources {
    pub path_dirs: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub session_ssh_host: Option<String>,
}

impl Sources {
    /// Resolve from tap's own environment. `cwd` is provided by the
    /// caller (the focused pane's cwd); it may be `None` (unreadable).
    pub fn from_env(cwd: Option<PathBuf>) -> Self {
        let path_dirs = std::env::var("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        Self {
            path_dirs,
            home: dirs::home_dir(),
            cwd,
            session_ssh_host: None,
        }
    }
}

/// One completion candidate. `display` is what the candidates row
/// shows; `insert` is the full word placed into the buffer (directory
/// candidates bake the trailing `/` in so the next Tab descends).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub display: String,
    pub insert: String,
    pub is_dir: bool,
}

/// Common shell builtins, so the first word completes without any
/// PATH hit (and for words that are builtins only).
const SHELL_BUILTINS: [&str; 30] = [
    "cd", "echo", "exit", "export", "history", "kill", "let", "local", "logout", "popd",
    "printf", "pushd", "pwd", "read", "return", "set", "shift", "source", "suspend",
    "test", "times", "trap", "true", "type", "typeset", "ulimit", "umask", "unalias",
    "unset", "wait",
];

/// Whether this word is a first-word command completion: starts the
/// line, no path separators, not a `~`/`$` expansion.
pub fn is_command_word(word_start_char: usize, word: &str) -> bool {
    word_start_char == 0
        && !word.is_empty()
        && !word.contains('/')
        && !word.starts_with('~')
        && !word.starts_with('$')
}

/// Candidates for a word. Dispatches on first-word-ness.
pub fn gather(word_start_char: usize, word: &str, sources: &Sources) -> Vec<Candidate> {
    if is_command_word(word_start_char, word) {
        command_candidates(word, &sources.path_dirs)
    } else {
        path_candidates(word, sources.cwd.as_deref(), sources.home.as_deref())
    }
}

/// Executables on PATH (plus builtins) matching the prefix. A command
/// shadowing (same basename in several dirs) appears once.
pub fn command_candidates(prefix: &str, path_dirs: &[PathBuf]) -> Vec<Candidate> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut candidates = Vec::new();

    for builtin in SHELL_BUILTINS {
        if builtin.starts_with(prefix) && seen.insert(builtin.to_string()) {
            candidates.push(Candidate {
                display: builtin.to_string(),
                insert: builtin.to_string(),
                is_dir: false,
            });
        }
    }

    for dir in path_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Ok(name) = name.into_string() else {
                continue;
            };
            if !name.starts_with(prefix) || !seen.insert(name.clone()) {
                continue;
            }
            if !is_executable(&entry.path()) {
                continue;
            }
            candidates.push(Candidate {
                display: name.clone(),
                insert: sh_escape(&name),
                is_dir: false,
            });
            if candidates.len() >= MAX_CANDIDATES {
                candidates.sort_by(|a, b| a.insert.cmp(&b.insert));
                return candidates;
            }
        }
    }

    candidates.sort_by(|a, b| a.insert.cmp(&b.insert));
    candidates
}

/// Filesystem candidates for a path word. Relative words anchor at the
/// pane cwd; `~` words match through the expanded home path but
/// complete back keeping the typed `~` head. Hidden entries complete
/// only once the component being typed starts with a dot.
pub fn path_candidates(
    word: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Vec<Candidate> {
    if word.is_empty() {
        return Vec::new();
    }

    // Split the *word* (never the joined path — joining mangles `.`
    // components): everything before the last `/` anchors the
    // directory, the trailing component is the name prefix.
    let (dir, prefix, head) = if word == "~" {
        let Some(home) = home else {
            return Vec::new();
        };
        (home.to_path_buf(), String::new(), String::from("~/"))
    } else {
        let split_at = word.rfind('/').map(|i| i + 1).unwrap_or(0);
        let (head, prefix) = word.split_at(split_at);
        let dir = if let Some(rest) = head.strip_prefix('~') {
            let Some(home) = home else {
                return Vec::new();
            };
            home.join(rest.strip_prefix('/').unwrap_or(rest))
        } else if head.starts_with('/') {
            PathBuf::from(head)
        } else {
            let Some(cwd) = cwd else {
                return Vec::new();
            };
            if head.is_empty() {
                cwd.to_path_buf()
            } else {
                cwd.join(head)
            }
        };
        (dir, prefix.to_string(), head.to_string())
    };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let hidden_ok = prefix.starts_with('.');

    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(name) = name.into_string() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        if !hidden_ok && name.starts_with('.') {
            continue;
        }
        if !name.starts_with(&prefix) {
            continue;
        }
        // Follow symlinks: a link pointing at a directory completes with
        // the `/` so the next Tab descends into it (Warp path.rs:62-70).
        // `DirEntry::file_type` does not follow, hence the metadata call.
        let is_dir = std::fs::metadata(entry.path())
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        let completed = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        candidates.push(Candidate {
            display: completed.clone(),
            // Replacement text is shell-escaped, display stays verbatim
            // (Warp splits the two — path.rs:275-336). The head joins the
            // escape pass: the typed word arrives unquoted from the
            // tokenizer, so it must be re-escaped together with the name.
            insert: sh_escape(&format!("{head}{completed}")),
            is_dir,
        });
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }

    candidates.sort_by(|a, b| a.insert.cmp(&b.insert));
    candidates
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// Longest common prefix in chars across all words. Empty slice or a
/// single word yields that word; disjoint words yield "".
pub fn common_prefix(words: &[&str]) -> String {
    let Some(first) = words.first() else {
        return String::new();
    };
    let mut len = first.chars().count();
    for word in &words[1..] {
        len = len.min(word.chars().count());
        let mut i = 0;
        for (a, b) in first.chars().zip(word.chars()) {
            if a != b {
                break;
            }
            i += 1;
        }
        len = len.min(i);
    }
    first.chars().take(len).collect()
}

// ---------------------------------------------------------------------------
// Dynamic completion: the Cobra `__complete` protocol + ssh routing.
//
// Cobra-based CLIs (kubectl, helm, gh, …) expose a hidden
// `__completeNoDesc` command: pass everything typed so far as argv plus
// the word being completed last (possibly empty), get one candidate
// per line and a trailing `:<bits>` directive. Querying the binary
// directly needs no shell integration; results may take an API round
// trip, so the caller spawns `spawn_dynamic` on a worker thread and
// polls the receiver.
// ---------------------------------------------------------------------------

/// CLIs known to speak the Cobra completion protocol. First word of the
/// line (after `command-aliases` resolution) must be one of these for
/// a dynamic query; unknown words keep today's static behavior.
const COBRA_TOOLS: [&str; 8] = [
    "argocd", "docker", "flux", "gh", "helm", "istioctl", "kind", "kubectl",
];

/// Wrappers whose destination can be parsed out of the line (or the
/// pane's foreground process cmdline) with plain argv rules.
const SSH_TOOLS: [&str; 2] = ["autossh", "ssh"];

/// A word of the line as the shell would see it: quotes stripped,
/// `start` is the char index of the word's first character (of the
/// raw span, quotes included, so it can drive buffer replacement).
#[derive(Debug, Clone, PartialEq)]
pub struct PToken {
    pub start: usize,
    pub text: String,
}

/// Shell-lite tokenizer: splits on whitespace outside quotes, handles
/// `'single'`, `"double"` (with `\"` / `\\` escapes) and backslash
/// escapes. Good enough for completion argv reconstruction; not a
/// shell grammar.
pub fn tokenize(chars: &[char]) -> Vec<PToken> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        let mut text = String::new();
        while i < chars.len() && !chars[i].is_whitespace() {
            match chars[i] {
                '\'' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        text.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1; // closing quote
                    }
                }
                '"' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\'
                            && i + 1 < chars.len()
                            && (chars[i + 1] == '"' || chars[i + 1] == '\\')
                        {
                            i += 1;
                        }
                        text.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1; // closing quote
                    }
                }
                '\\' if i + 1 < chars.len() => {
                    text.push(chars[i + 1]);
                    i += 2;
                }
                c => {
                    text.push(c);
                    i += 1;
                }
            }
        }
        tokens.push(PToken { start, text });
    }
    tokens
}

/// Where an `ssh`-family command line points: the destination slot and
/// its text, plus where the remote command argv starts (if any).
#[derive(Debug, Clone, PartialEq)]
pub struct SshRoute {
    pub dest_slot: usize,
    pub dest: String,
    pub remote_start: Option<usize>,
}

/// Parse `ssh [flags] [--] host [remote command...]`. Returns `None`
/// when the line is not an ssh invocation. `dest_slot` is where the
/// destination belongs even when nothing was typed there yet (all
/// flags consumed), so `ssh <Tab>` still offers the host table. The
/// command name matches on its basename — wrappers arrive as full
/// paths in `/proc` cmdlines (`/usr/lib/autossh/autossh …`).
pub fn ssh_route(argv: &[String]) -> Option<SshRoute> {
    if argv.is_empty() || !SSH_TOOLS.contains(&command_basename(&argv[0]).as_str()) {
        return None;
    }
    let mut i = 1;
    let mut after_dashdash = false;
    while i < argv.len() {
        let tok = &argv[i];
        if !after_dashdash {
            if tok == "--" {
                after_dashdash = true;
                i += 1;
                continue;
            }
            if tok.starts_with('-') && tok.len() > 1 {
                i += if ssh_flag_takes_value(tok) { 2 } else { 1 };
                continue;
            }
        }
        // First non-flag token is the destination.
        return Some(SshRoute {
            dest_slot: i,
            dest: tok.clone(),
            remote_start: Some(i + 1),
        });
    }
    // Only flags so far: the destination slot is the next word.
    Some(SshRoute {
        dest_slot: argv.len(),
        dest: String::new(),
        remote_start: Some(argv.len()),
    })
}

/// Short ssh flags whose value may arrive as the next token. Values
/// already glued to the flag (`-p2222`, `-oOption=…`) are single
/// tokens — the caller only skips the next word when the token is
/// exactly two chars.
fn ssh_flag_takes_value(tok: &str) -> bool {
    if tok.starts_with("--") || tok.len() > 2 {
        return false;
    }
    matches!(
        tok.chars().nth(1),
        Some(
            'b' | 'c'
                | 'D'
                | 'e'
                | 'F'
                | 'i'
                | 'J'
                | 'L'
                | 'l'
                | 'M'
                | 'm'
                | 'o'
                | 'p'
                | 'R'
                | 'W'
                | 'w'
        )
    )
}

/// Resolve a word to a Cobra tool name: explicit alias mapping first
/// (`k` → `kubectl`), then the raw name — matched on the basename, so
/// a fully qualified `/usr/local/bin/kubectl` also routes dynamically.
pub fn resolve_tool(word: &str, aliases: &HashMap<String, String>) -> Option<String> {
    if let Some(target) = aliases.get(word) {
        if COBRA_TOOLS.contains(&target.as_str()) {
            return Some(target.clone());
        }
    }
    if COBRA_TOOLS.contains(&command_basename(word).as_str()) {
        return Some(command_basename(word));
    }
    None
}

/// Last path component of a command token (`/usr/lib/autossh/autossh`
/// → `autossh`, plain words pass through).
fn command_basename(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_string()
}

/// Where a Tab press routes to, decided from the argv before the word.
#[derive(Debug)]
pub enum Route {
    /// First bare word of a local line — PATH + builtin completion.
    Command,
    /// Filesystem path completion against the pane cwd.
    Path,
    /// The word sits in an ssh/autossh destination slot.
    SshHosts,
    /// A dynamic `__complete`-style query (local or over ssh).
    Dynamic(DynamicRequest),
    /// Nothing to offer (e.g. a remote word we don't know how to ask).
    None,
}

/// Route a Tab press. `argv` holds the unquoted words before the word
/// being completed, `word_idx` its token index (`argv.len()` when the
/// word is empty), `word_start` its char position.
pub fn route(
    argv: &[String],
    word_idx: usize,
    word: &str,
    word_start: usize,
    aliases: &HashMap<String, String>,
    session_ssh_host: Option<&str>,
) -> Route {
    // One-shot `ssh host cmd …` line: host slot completes statically,
    // remote words query the remote host.
    if let Some(first) = argv.first() {
        if SSH_TOOLS.contains(&first.as_str()) {
            let Some(ssh) = ssh_route(argv) else {
                return Route::None;
            };
            if word_idx == ssh.dest_slot {
                return Route::SshHosts;
            }
            let Some(remote_start) = ssh.remote_start else {
                return Route::None;
            };
            if word_idx < remote_start {
                return Route::None;
            }
            let remote_argv = &argv[remote_start..word_idx];
            if remote_argv.is_empty() {
                return Route::Dynamic(DynamicRequest::RemoteCommands {
                    host: ssh.dest,
                    word: word.to_string(),
                });
            }
            return match resolve_tool(&remote_argv[0], aliases) {
                Some(tool) => Route::Dynamic(DynamicRequest::Cobra {
                    tool,
                    args: remote_argv[1..].to_vec(),
                    word: word.to_string(),
                    host: Some(ssh.dest),
                }),
                None => Route::None,
            };
        }
    }

    // Inside an interactive ssh session: everything is remote. First
    // word → remote command names, then Cobra tools query remotely.
    // Unknown shapes stay silent (no remote path completion).
    if let Some(host) = session_ssh_host {
        if argv.is_empty() {
            return Route::Dynamic(DynamicRequest::RemoteCommands {
                host: host.to_string(),
                word: word.to_string(),
            });
        }
        return match resolve_tool(&argv[0], aliases) {
            Some(tool) => Route::Dynamic(DynamicRequest::Cobra {
                tool,
                args: argv[1..].to_vec(),
                word: word.to_string(),
                host: Some(host.to_string()),
            }),
            None => Route::None,
        };
    }

    // Local line.
    if argv.is_empty() {
        if is_command_word(word_start, word) {
            Route::Command
        } else {
            Route::Path
        }
    } else if let Some(tool) = resolve_tool(&argv[0], aliases) {
        Route::Dynamic(DynamicRequest::Cobra {
            tool,
            args: argv[1..].to_vec(),
            word: word.to_string(),
            host: None,
        })
    } else {
        Route::Path
    }
}

/// A dynamic completion query. `Cobra` runs `<tool> __completeNoDesc
/// <args…> <word>`, optionally through ssh; `RemoteCommands` lists
/// command names on a remote host (compgen).
#[derive(Debug)]
pub enum DynamicRequest {
    Cobra {
        tool: String,
        args: Vec<String>,
        word: String,
        host: Option<String>,
    },
    RemoteCommands {
        host: String,
        word: String,
    },
}

impl DynamicRequest {
    /// Stable identity of the query: equal requests share cache slots
    /// and let a late result be matched against the live buffer state.
    pub fn key(&self) -> String {
        match self {
            DynamicRequest::Cobra {
                tool,
                args,
                word,
                host,
            } => {
                let mut key = String::from("c");
                if let Some(host) = host {
                    key.push('\u{1}');
                    key.push_str(host);
                }
                key.push('\u{1}');
                key.push_str(tool);
                for arg in args {
                    key.push('\u{1}');
                    key.push_str(arg);
                }
                key.push('\u{1}');
                key.push_str(word);
                key
            }
            DynamicRequest::RemoteCommands { host, word } => {
                format!("r\u{1}{host}\u{1}{word}")
            }
        }
    }

    /// Remote queries pay an ssh handshake on top of the tool's own
    /// work, so they get a longer watchdog deadline.
    fn is_remote(&self) -> bool {
        match self {
            DynamicRequest::Cobra { host: Some(_), .. } => true,
            DynamicRequest::RemoteCommands { .. } => true,
            DynamicRequest::Cobra { host: None, .. } => false,
        }
    }
}

/// What a finished dynamic query hands back to the UI thread.
pub struct DynamicResult {
    pub candidates: Vec<Candidate>,
    /// `:2` directive — do not append a space after a single match.
    pub no_space: bool,
}

/// POSIX single-quote a word for a remote shell line built by ssh.
pub fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return String::from("''");
    }
    let safe = s.bytes().all(|b| {
        matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-')
    });
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Backslash-escape a completion word for insertion into the buffer.
/// Unlike [`sh_quote`] (whole-word single quoting for remote shell
/// lines) this keeps the text unquoted, so a leading `~/` still tilde
/// expands and the insert stays visually close to the typed form —
/// Warp's completion replacement style (warp_util/path.rs `escape`).
/// Non-ASCII passes through: shell metacharacters are all ASCII.
pub fn sh_escape(s: &str) -> String {
    fn escape_char(c: char, out: &mut String) {
        let safe = c.is_ascii_alphanumeric()
            || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
            || !c.is_ascii();
        if safe {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    let mut escaped = String::with_capacity(s.len());
    // A leading `~` must stay unquoted for tilde expansion (Warp's
    // `shell_escape` special case).
    if let Some(rest) = s.strip_prefix("~/") {
        escaped.push_str("~/");
        for c in rest.chars() {
            escape_char(c, &mut escaped);
        }
    } else {
        for c in s.chars() {
            escape_char(c, &mut escaped);
        }
    }
    escaped
}

/// The unquoted text of a shell-escaped word, as the completion
/// tokenizer sees it — the inverse of [`sh_escape`] for cycle-state
/// bookkeeping (the bar compares candidates against tokenized words).
pub fn unescape_word(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    tokenize(&chars)
        .first()
        .map(|token| token.text.clone())
        .unwrap_or_default()
}

/// Watchdog deadlines: local `__complete` is one binary spawn; remote
/// adds an ssh handshake and the tool's own API round trip.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_TIMEOUT: Duration = Duration::from_secs(12);

/// Run a dynamic query on a detached worker thread. The receiver
/// yields exactly one result — candidates (possibly empty) or nothing
/// when the channel drops after a spawn failure. The watchdog kills
/// the child past the deadline, which also closes the stdout pipe, so
/// the worker always finishes on its own and a stale receiver is safe
/// to drop.
pub fn spawn_dynamic(request: DynamicRequest) -> mpsc::Receiver<DynamicResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now()
            + if request.is_remote() {
                REMOTE_TIMEOUT
            } else {
                LOCAL_TIMEOUT
            };

        let Some(mut command) = build_command(&request) else {
            return;
        };
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let Ok(mut child) = command.spawn() else {
            return; // unknown binary / spawn failure — stays silent
        };
        let mut stdout = child.stdout.take();

        let shared = Arc::new(Mutex::new(child));
        let watchdog = Arc::clone(&shared);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(50));
            let Ok(mut guard) = watchdog.lock() else {
                return;
            };
            match guard.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => return,
            }
            if Instant::now() >= deadline {
                let _ = guard.kill();
                return;
            }
        });

        // read_to_end returns at EOF — process exit or watchdog kill,
        // whichever comes first. Candidates are small; no pipe-full
        // deadlock because the watchdog bounds the child's lifetime.
        let mut raw = Vec::new();
        if let Some(stream) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(stream, &mut raw);
        }
        let raw = String::from_utf8_lossy(&raw).into_owned();
        {
            let Ok(mut guard) = shared.lock() else {
                return;
            };
            let _ = guard.wait(); // reap
        }

        let (candidates, no_space) = parse_complete_output(&raw);
        tx.send(DynamicResult {
            candidates,
            no_space,
        })
        .ok();
    });
    rx
}

fn build_command(request: &DynamicRequest) -> Option<std::process::Command> {
    match request {
        DynamicRequest::Cobra {
            tool,
            args,
            word,
            host: None,
        } => {
            let mut command = std::process::Command::new(tool);
            command.arg("__completeNoDesc").args(args).arg(word);
            Some(command)
        }
        DynamicRequest::Cobra {
            tool,
            args,
            word,
            host: Some(host),
        } => {
            let mut remote = vec![sh_quote(tool), String::from("__completeNoDesc")];
            remote.extend(args.iter().map(|a| sh_quote(a)));
            remote.push(sh_quote(word));
            Some(ssh_command(host, &remote.join(" ")))
        }
        DynamicRequest::RemoteCommands { host, word } => {
            let script = format!(
                "bash -c 'compgen -A command -- \"$1\" 2>/dev/null' _ {}",
                sh_quote(word)
            );
            Some(ssh_command(host, &script))
        }
    }
}

/// A side-channel ssh that never prompts: password-less auth only and
/// a connect deadline, so a slow or locked-down host fails fast. The
/// connections multiplex over a shared ControlMaster — the first query
/// to a host pays the handshake (TCP + KEX + auth), everything within
/// `ControlPersist` reuses the channel in milliseconds. Without this
/// every Tab press opened a fresh connection, which read as "ssh
/// completion is slow" on any host with real RTT.
fn ssh_command(host: &str, remote_line: &str) -> std::process::Command {
    let mut command = std::process::Command::new("ssh");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=3")
        .arg("-o")
        .arg("ControlMaster=auto");
    if let Some(dir) = mux_socket_dir() {
        // `%C` is ssh's own hash of the connection parameters: short
        // (immune to the unix socket path limit) and one socket per
        // destination. `auto` + ssh's socket perms make stale sockets
        // and concurrent taps racing here both safe.
        command
            .arg("-o")
            .arg(format!("ControlPath={}/%C", dir.to_string_lossy()))
            .arg("-o")
            .arg("ControlPersist=10m");
    }
    command.arg(host).arg(remote_line);
    command
}

/// `{cache}/rio/ssh-mux`, created once. Falls back to plain
/// per-press connections when the cache dir is unavailable.
fn mux_socket_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = dirs::cache_dir()?.join("rio").join("ssh-mux");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_deref()
}

/// Parse `__complete` output: candidate lines (a `\t` description is
/// stripped) and the trailing `:<bits>` directive. Directive bits we
/// honour: `2` no space after a single match, `16` candidates are
/// directories (bake the trailing `/`), `32` keep the tool's order —
/// otherwise candidates sort by insert text. Bit `4` (no file
/// fallback) is enforced by the caller not gathering static
/// candidates in dynamic contexts at all.
pub fn parse_complete_output(stdout: &str) -> (Vec<Candidate>, bool) {
    let mut directive = 0u32;
    let mut names = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // Cobra injects contextual hints on stdout; its own completion
        // scripts filter them out, so do we.
        if line.starts_with("_activeHelp_") {
            continue;
        }
        if let Some(bits) = line.strip_prefix(':') {
            directive = bits.trim().parse().unwrap_or(0);
        } else {
            let name = line.split('\t').next().unwrap_or(line);
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    let no_space = directive & 2 != 0;
    let is_dir = directive & 16 != 0;
    let keep_order = directive & 32 != 0;

    let mut candidates: Vec<Candidate> = names
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|name| {
            // Same replacement/display split as path candidates: the
            // insert is shell-escaped (a value with spaces stays one
            // word), the display keeps the tool's raw text.
            let word = if is_dir && !name.ends_with('/') {
                format!("{name}/")
            } else {
                name.clone()
            };
            Candidate {
                display: word.clone(),
                insert: sh_escape(&word),
                is_dir,
            }
        })
        .collect();
    if !keep_order {
        candidates.sort_by(|a, b| a.insert.cmp(&b.insert));
    }
    (candidates, no_space)
}

/// Host candidates for an ssh destination slot: non-wildcard `Host`
/// patterns from `~/.ssh/config` plus plain (non-hashed) entries from
/// `~/.ssh/known_hosts`. A `user@` head is kept in front of the match.
pub fn ssh_host_candidates(word: &str, home: Option<&Path>) -> Vec<Candidate> {
    let Some(home) = home else {
        return Vec::new();
    };
    let (user_head, prefix) = match word.rsplit_once('@') {
        Some((user, host)) => (format!("{user}@"), host),
        None => (String::new(), word),
    };

    let mut hosts: Vec<String> = Vec::new();
    if let Ok(config) = std::fs::read_to_string(home.join(".ssh").join("config")) {
        for line in config.lines() {
            let mut parts = line.split_whitespace();
            if parts
                .next()
                .is_some_and(|kw| kw.eq_ignore_ascii_case("host"))
            {
                for pattern in parts {
                    // Wildcards and negations describe groups, not
                    // destinations.
                    if !pattern.contains(['*', '?', '!']) {
                        hosts.push(pattern.to_string());
                    }
                }
            }
        }
    }
    if let Ok(known) = std::fs::read_to_string(home.join(".ssh").join("known_hosts")) {
        for line in known.lines() {
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            if first.starts_with('|') {
                continue; // hashed entry (HashKnownHosts) — unreadable
            }
            for entry in first.split(',') {
                let host = entry
                    .strip_prefix('[')
                    .map(|rest| rest.split(']').next().unwrap_or(rest))
                    .unwrap_or(entry);
                if !host.is_empty() {
                    hosts.push(host.to_string());
                }
            }
        }
    }

    hosts.sort();
    hosts.dedup();
    let mut candidates = Vec::new();
    for host in hosts {
        if host.starts_with(prefix) {
            candidates.push(Candidate {
                display: host.clone(),
                insert: format!("{user_head}{host}"),
                is_dir: false,
            });
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 =
                std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("tap-completion-{}-{n}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sources_with(cwd: Option<PathBuf>, home: Option<PathBuf>) -> Sources {
        Sources {
            path_dirs: Vec::new(),
            home,
            cwd,
            session_ssh_host: None,
        }
    }

    fn inserts(candidates: Vec<Candidate>) -> Vec<String> {
        candidates.into_iter().map(|c| c.insert).collect()
    }

    #[test]
    fn command_word_detection() {
        assert!(is_command_word(0, "gi"));
        assert!(!is_command_word(3, "gi"), "not the first word");
        assert!(!is_command_word(0, "src/f"), "contains a slash");
        assert!(!is_command_word(0, "~/f"), "tilde form");
        assert!(!is_command_word(0, "$VA"), "variable form");
        assert!(!is_command_word(0, ""), "empty word");
    }

    #[test]
    fn builtin_completion_matches_prefix() {
        let sources = sources_with(None, None);
        assert_eq!(inserts(gather(0, "sour", &sources)), vec!["source"]);
    }

    #[test]
    fn path_completion_relative_hidden_and_dirs() {
        let dir = TempDir::new("relative");
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("foo.txt"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        std::fs::create_dir(dir.path().join(".config")).unwrap();
        let sources = sources_with(Some(dir.path().to_path_buf()), None);

        // Plain prefix: visible entries only.
        assert_eq!(inserts(gather(2, "f", &sources)), vec!["foo.txt"]);
        // Dir candidates bake in the slash so the next Tab descends.
        assert_eq!(inserts(gather(2, "s", &sources)), vec!["src/"]);
        // A leading dot unlocks hidden entries.
        assert_eq!(
            inserts(gather(2, ".", &sources)),
            vec![".config/", ".hidden"]
        );
        // Inside a typed directory part: contents complete relative.
        assert!(inserts(gather(2, "src/", &sources)).is_empty());
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();
        assert_eq!(inserts(gather(2, "src/", &sources)), vec!["src/main.rs"]);
    }

    #[test]
    fn path_completion_absolute_and_tilde() {
        let dir = TempDir::new("absolute");
        std::fs::write(dir.path().join("alpha"), "").unwrap();
        let sources = sources_with(None, None);

        let word = format!("{}/al", dir.path().display());
        assert_eq!(
            inserts(gather(2, &word, &sources)),
            vec![format!("{}/alpha", dir.path().display())]
        );
        // Trailing slash lists the directory itself.
        let word = format!("{}/", dir.path().display());
        assert_eq!(
            inserts(gather(2, &word, &sources)),
            vec![format!("{}/alpha", dir.path().display())]
        );

        // `~` completes through the expanded home path, keeping the
        // typed short head.
        let home = TempDir::new("home");
        std::fs::write(home.path().join("zshrc"), "").unwrap();
        let sources = sources_with(None, Some(home.path().to_path_buf()));
        assert_eq!(inserts(gather(0, "~/zs", &sources)), vec!["~/zshrc"]);
        assert_eq!(inserts(gather(0, "~", &sources)), vec!["~/zshrc"]);
    }

    #[test]
    fn common_prefix_basics() {
        assert_eq!(common_prefix(&["foobar", "footbar", "foo"]), "foo");
        assert_eq!(common_prefix(&["abc"]), "abc");
        assert_eq!(common_prefix(&["abc", "xyz"]), "");
        assert_eq!(common_prefix(&["中文测试", "中文编辑"]), "中文");
        assert_eq!(common_prefix(&[]), "");
    }

    fn tokens(line: &str) -> Vec<PToken> {
        let chars: Vec<char> = line.chars().collect();
        tokenize(&chars)
    }

    fn token_texts(line: &str) -> Vec<String> {
        tokens(line).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn tokenizer_splits_and_strips_quotes() {
        let toks = tokens(r#"kubectl get "my pod" 'a b' esc\ ape --flag=v"#);
        let texts: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
        assert_eq!(
            texts,
            vec!["kubectl", "get", "my pod", "a b", "esc ape", "--flag=v"]
        );
        // Raw spans keep the quote characters: replacement needs them.
        assert_eq!(toks[2].start, 12);
        assert_eq!(toks[3].start, 21);
        // Unterminated quote still yields the opened content.
        assert_eq!(token_texts(r#"echo "hel"#), vec!["echo", "hel"]);
    }

    #[test]
    fn ssh_route_finds_destination_and_remote_start() {
        let argv = |s: &str| token_texts(s);
        let route = ssh_route(&argv("ssh -p 2222 deploy@web1 kubectl get pod")).unwrap();
        assert_eq!(route.dest_slot, 3);
        assert_eq!(route.dest, "deploy@web1");
        assert_eq!(route.remote_start, Some(4));

        // Value glued to the flag stays one token.
        let route = ssh_route(&argv("ssh -p2222 web1")).unwrap();
        assert_eq!(route.dest_slot, 2);
        assert_eq!(route.remote_start, Some(3));

        // `--` terminates option parsing.
        let route = ssh_route(&argv("autossh -v -- -weird-host ls")).unwrap();
        assert_eq!(route.dest, "-weird-host");

        // Not ssh / destination not typed yet (hosts still offered).
        assert!(ssh_route(&argv("kubectl get pod")).is_none());
        let route = ssh_route(&argv("ssh -p 2222")).unwrap();
        assert_eq!(route.dest_slot, 3);
        assert_eq!(route.dest, "");
    }

    fn no_aliases() -> HashMap<String, String> {
        HashMap::new()
    }

    fn aliases() -> HashMap<String, String> {
        HashMap::from([("k".to_string(), "kubectl".to_string())])
    }

    #[test]
    fn route_dispatches_local_lines() {
        let argv = |s: &str| token_texts(s);

        assert!(matches!(
            route(&argv(""), 0, "ku", 0, &no_aliases(), None),
            Route::Command
        ));
        // Non-command first word (`$VA`, `./x`) falls to Path.
        assert!(matches!(
            route(&argv(""), 0, "$VA", 0, &no_aliases(), None),
            Route::Path
        ));
        // Plain tool at a later word → local dynamic query.
        match route(&argv("kubectl get"), 2, "pod", 11, &no_aliases(), None) {
            Route::Dynamic(DynamicRequest::Cobra {
                tool,
                args,
                word,
                host,
            }) => {
                assert_eq!(tool, "kubectl");
                assert_eq!(args, vec!["get"]);
                assert_eq!(word, "pod");
                assert_eq!(host, None);
            }
            other => panic!("expected local dynamic route, got {other:?}?"),
        }
        // Unknown first word keeps static path completion.
        assert!(matches!(
            route(&argv("ls -la"), 2, "sr", 6, &no_aliases(), None),
            Route::Path
        ));
    }

    #[test]
    fn ssh_route_matches_wrapper_paths_from_proc_cmdlines() {
        // Debian's autossh keeps a resolved script path as argv[0] —
        // exactly what the session probe reads from /proc.
        let argv = vec![
            "/usr/lib/autossh/autossh".to_string(),
            "-M".to_string(),
            "0".to_string(),
            "172.24.0.145".to_string(),
        ];
        let route = ssh_route(&argv).unwrap();
        assert_eq!(route.dest, "172.24.0.145");
        assert_eq!(route.dest_slot, 3);

        // Fully qualified ssh in a one-shot line still parses.
        let route = ssh_route(&["/usr/bin/ssh".to_string(), "web1".to_string()]).unwrap();
        assert_eq!(route.dest, "web1");
        // Non-ssh paths stay unmatched.
        assert!(ssh_route(&["/usr/bin/tail".to_string(), "-f".to_string()]).is_none());
    }

    #[test]
    fn route_resolves_aliases_to_cobra_tools() {
        assert_eq!(resolve_tool("k", &aliases()).as_deref(), Some("kubectl"));
        assert_eq!(
            resolve_tool("kubectl", &no_aliases()).as_deref(),
            Some("kubectl")
        );
        assert_eq!(resolve_tool("k", &no_aliases()), None);
        // A fully qualified Cobra binary routes too.
        assert_eq!(
            resolve_tool("/usr/local/bin/kubectl", &no_aliases()).as_deref(),
            Some("kubectl")
        );
        // An alias pointing at a non-Cobra tool is ignored.
        let plain = HashMap::from([("k".to_string(), "ls".to_string())]);
        assert_eq!(resolve_tool("k", &plain), None);
    }

    #[test]
    fn route_dispatches_one_shot_ssh_lines() {
        let argv = |s: &str| token_texts(s);

        // Host slot → static hosts.
        assert!(matches!(
            route(&argv("ssh"), 1, "de", 4, &no_aliases(), None),
            Route::SshHosts
        ));
        assert!(matches!(
            route(&argv("ssh -p 2222"), 3, "user@", 11, &no_aliases(), None),
            Route::SshHosts
        ));

        // First remote word → remote command names.
        match route(&argv("ssh web1"), 2, "", 8, &no_aliases(), None) {
            Route::Dynamic(DynamicRequest::RemoteCommands { host, word }) => {
                assert_eq!(host, "web1");
                assert_eq!(word, "");
            }
            other => panic!("expected remote commands route, got {other:?}?"),
        }

        // Remote Cobra tool → remote dynamic query.
        match route(
            &argv("ssh web1 kubectl get pod"),
            5,
            "my-",
            21,
            &no_aliases(),
            None,
        ) {
            Route::Dynamic(DynamicRequest::Cobra {
                tool, host, word, ..
            }) => {
                assert_eq!(tool, "kubectl");
                assert_eq!(host.as_deref(), Some("web1"));
                assert_eq!(word, "my-");
            }
            other => panic!("expected remote cobra route, got {other:?}?"),
        }

        // Remote non-Cobra tool stays silent (no remote path completion).
        assert!(matches!(
            route(&argv("ssh web1 tail -f"), 4, "log", 16, &no_aliases(), None),
            Route::None
        ));
    }

    #[test]
    fn route_dispatches_inside_ssh_sessions() {
        let argv = |s: &str| token_texts(s);

        // First word in the remote shell → remote command names.
        match route(&argv(""), 0, "kt", 0, &no_aliases(), Some("web1")) {
            Route::Dynamic(DynamicRequest::RemoteCommands { host, word }) => {
                assert_eq!(host, "web1");
                assert_eq!(word, "kt");
            }
            other => panic!("expected remote commands route, got {other:?}?"),
        }

        // Remote Cobra query, alias-resolved.
        match route(&argv("k get pod"), 3, "", 9, &aliases(), Some("web1")) {
            Route::Dynamic(DynamicRequest::Cobra { tool, host, .. }) => {
                assert_eq!(tool, "kubectl");
                assert_eq!(host.as_deref(), Some("web1"));
            }
            other => panic!("expected remote cobra route, got {other:?}?"),
        }

        // Unknown remote word: silent.
        assert!(matches!(
            route(&argv("tail"), 1, "f", 5, &no_aliases(), Some("web1")),
            Route::None
        ));
    }

    #[test]
    fn dynamic_keys_identify_requests() {
        let a = DynamicRequest::Cobra {
            tool: "kubectl".into(),
            args: vec!["get".into(), "pod".into()],
            word: String::new(),
            host: None,
        };
        let b = DynamicRequest::Cobra {
            tool: "kubectl".into(),
            args: vec!["get".into(), "pod".into()],
            word: String::new(),
            host: None,
        };
        let remote = DynamicRequest::Cobra {
            tool: "kubectl".into(),
            args: vec![],
            word: String::new(),
            host: Some("web1".into()),
        };
        let commands = DynamicRequest::RemoteCommands {
            host: "web1".into(),
            word: String::new(),
        };
        assert_eq!(a.key(), b.key());
        assert_ne!(a.key(), remote.key());
        assert_ne!(remote.key(), commands.key());
    }

    #[test]
    fn sh_quote_passes_safe_and_wraps_rest() {
        assert_eq!(sh_quote("my-pod_1.2"), "my-pod_1.2");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn sh_escape_backslash_escapes_metacharacters() {
        assert_eq!(sh_escape("plain-pod_1.2"), "plain-pod_1.2");
        assert_eq!(sh_escape("my file"), r"my\ file");
        assert_eq!(sh_escape("it's"), r"it\'s");
        assert_eq!(sh_escape("a$b"), r"a\$b");
        // Leading tilde stays expandable; a mid-word tilde is literal.
        assert_eq!(sh_escape("~/my dir"), "~/my\\ dir");
        assert_eq!(sh_escape("a~b"), r"a\~b");
        // Non-ASCII passes through — shell metacharacters are ASCII.
        assert_eq!(sh_escape("中文 文件"), "中文\\ 文件");
        assert_eq!(unescape_word(&sh_escape("my file $dir")), "my file $dir");
        assert_eq!(unescape_word(&sh_escape("~/a b")), "~/a b");
    }

    #[test]
    fn path_candidates_escape_inserts_keep_display() {
        let dir = TempDir::new("escaping");
        std::fs::write(dir.path().join("my file.txt"), "").unwrap();
        std::fs::write(dir.path().join("it's here"), "").unwrap();
        let sources = sources_with(Some(dir.path().to_path_buf()), None);

        let candidates = gather(2, "my", &sources);
        assert_eq!(candidates[0].display, "my file.txt", "display verbatim");
        assert_eq!(candidates[0].insert, r"my\ file.txt", "insert escaped");
        assert_eq!(unescape_word(&candidates[0].insert), "my file.txt");

        // The quote character is escaped too.
        let candidates = gather(2, "it", &sources);
        assert_eq!(candidates[0].insert, "it\\'s\\ here");
    }

    #[test]
    fn symlinked_directories_complete_with_separator() {
        let dir = TempDir::new("symlink");
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link"))
            .unwrap();
        let sources = sources_with(Some(dir.path().to_path_buf()), None);

        // The symlink is followed for the directory test, so it gets
        // the trailing slash like a real directory.
        assert_eq!(inserts(gather(2, "li", &sources)), vec!["link/"]);
        // A broken symlink falls back to file treatment (no slash).
        std::os::unix::fs::symlink(
            dir.path().join("nowhere"),
            dir.path().join("dangling"),
        )
        .unwrap();
        assert_eq!(inserts(gather(2, "da", &sources)), vec!["dangling"]);
    }

    #[test]
    fn parse_output_reads_candidates_and_directives() {
        let (candidates, no_space) = parse_complete_output("zebra\napple\n:4\n");
        assert!(!no_space);
        assert_eq!(inserts(candidates), vec!["apple", "zebra"]); // sorted

        // Descriptions after a tab are dropped.
        let (candidates, _) = parse_complete_output("beta\tsome description\n:2\n");
        assert!(!candidates[0].is_dir);
        assert_eq!(inserts(candidates), vec!["beta"]);

        // `:2` = no space after single match.
        let (_, no_space) = parse_complete_output("x\n:2\n");
        assert!(no_space);

        // `:16` = directory candidates get the trailing slash.
        let (candidates, _) = parse_complete_output("var/log\n:16\n");
        assert!(candidates[0].is_dir);
        assert_eq!(inserts(candidates), vec!["var/log/"]);

        // `:32` = keep the tool's order.
        let (candidates, _) = parse_complete_output("zebra\napple\n:36\n");
        assert_eq!(inserts(candidates), vec!["zebra", "apple"]);

        // Empty / directive-only output yields nothing.
        assert_eq!(
            inserts(parse_complete_output(":4\n").0),
            Vec::<String>::new()
        );
        assert_eq!(inserts(parse_complete_output("").0), Vec::<String>::new());

        // Cobra's ActiveHelp hints ride stdout — filtered like the
        // official completion scripts do.
        let (candidates, _) = parse_complete_output(
            "_activeHelp_ This command does not take any more arguments\nnginx\n:4\n",
        );
        assert_eq!(inserts(candidates), vec!["nginx"]);

        // Values with spaces insert shell-escaped, display verbatim.
        let (candidates, _) = parse_complete_output("my pod\n:4\n");
        assert_eq!(candidates[0].display, "my pod");
        assert_eq!(inserts(candidates), vec![r"my\ pod"]);
    }

    #[test]
    fn ssh_hosts_from_config_and_known_hosts() {
        let home = TempDir::new("ssh-home");
        let ssh_dir = home.path().join(".ssh");
        std::fs::create_dir(&ssh_dir).unwrap();
        std::fs::write(
            ssh_dir.join("config"),
            "Host web1 web2\n  HostName 10.0.0.1\n\nHost *.corp\n  User deploy\nMatch host * prod\n  ForwardAgent no\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("known_hosts"),
            "|1|hashed|entry ssh-ed25519 AAAA\nweb1,10.0.0.9 ssh-ed25519 AAAA\n[bastion]:2222 ssh-ed25519 AAAA\n",
        )
        .unwrap();
        let home = Some(home.path().to_path_buf());

        assert_eq!(
            inserts(ssh_host_candidates("", home.as_deref())),
            vec!["10.0.0.9", "bastion", "web1", "web2"]
        );
        assert_eq!(
            inserts(ssh_host_candidates("w", home.as_deref())),
            vec!["web1", "web2"]
        );
        // `user@` head is preserved.
        assert_eq!(
            inserts(ssh_host_candidates("deploy@web", home.as_deref())),
            vec!["deploy@web1", "deploy@web2"]
        );
        assert_eq!(
            inserts(ssh_host_candidates("zz", home.as_deref())),
            Vec::<String>::new()
        );
    }

    #[test]
    fn spawn_dynamic_reports_missing_binaries_as_empty() {
        let rx = spawn_dynamic(DynamicRequest::Cobra {
            tool: "tap-no-such-binary-4f2a".into(),
            args: vec![],
            word: String::new(),
            host: None,
        });
        // A spawn failure drops the sender without sending: recv fails,
        // and that is the contract — silent, never blocking.
        if let Ok(result) = rx.recv() {
            assert_eq!(inserts(result.candidates), Vec::<String>::new());
        }
    }

    /// End-to-end pipeline against a live ssh host (key auth + kubectl
    /// installed required): `route` picks the remote query, the worker
    /// spawns the side channel, the parser yields command names. Run
    /// explicitly: `cargo test -p rioterm e2e -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs network + key auth to the test host"]
    fn e2e_remote_commands_pipeline_reaches_a_live_host() {
        let host =
            std::env::var("TAP_E2E_SSH_HOST").unwrap_or_else(|_| "172.24.0.145".into());
        // In an ssh session the buffer is just "kube": empty argv in
        // front, session host carried by Sources, not the line.
        let request = match route(&[], 0, "kube", 0, &no_aliases(), Some(&host)) {
            Route::Dynamic(request) => request,
            other => panic!("expected dynamic route, got {other:?}"),
        };
        let rx = spawn_dynamic(request);
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("worker answers");
        let names = inserts(result.candidates);
        println!("remote candidates for kube: {names:?}");
        assert!(
            names.iter().any(|n| n.starts_with("kube")),
            "expected kube* commands from the remote host, got {names:?}"
        );
    }

    #[test]
    fn ssh_side_channel_multiplexes_over_a_control_master() {
        let rendered = format!("{:?}", ssh_command("user@host", "compgen -A command"));
        assert!(rendered.contains("BatchMode=yes"));
        assert!(rendered.contains("ConnectTimeout=3"));
        assert!(rendered.contains("ControlMaster=auto"));
        // The mux pair rides along whenever the cache dir resolved —
        // which it does in a normal test environment.
        if mux_socket_dir().is_some() {
            assert!(rendered.contains("ControlPersist=10m"));
            assert!(rendered.contains("ControlPath="));
        }
    }
}
