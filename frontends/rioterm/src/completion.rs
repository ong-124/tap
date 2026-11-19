// Tab-completion candidate gathering for the unified input bar. Pure
// functions over an injected environment — no process-global state —
// so tests build `Sources` by hand instead of touching `PATH`.
//
// Two modes: the first word of the line completes against commands
// (PATH executables + a builtin table); any other word completes
// filesystem paths anchored at the focused pane's cwd.

use std::path::{Path, PathBuf};

/// Hard cap on returned candidates; keeps a bare-word Tab at the root
/// of a huge directory from flooding the candidates row.
const MAX_CANDIDATES: usize = 100;

/// Everything `gather` needs, resolved once per Tab press by the
/// caller (screen layer): PATH for command mode, home for `~`
/// expansion, and the focused pane's cwd for relative paths.
pub struct Sources {
    pub path_dirs: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
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
        }
    }
}

/// One completion candidate. `display` is what the candidates row
/// shows; `insert` is the full word placed into the buffer (directory
/// candidates bake the trailing `/` in so the next Tab descends).
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
                insert: name,
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
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let completed = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        candidates.push(Candidate {
            display: completed.clone(),
            insert: format!("{head}{completed}"),
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
}
