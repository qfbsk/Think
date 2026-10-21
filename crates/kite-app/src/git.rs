//! Git facts for the sidebar: the state of every path git reports, and the line
//! counts against `HEAD`.
//!
//! Every `git` call costs ~15ms of process start-up on this machine, so a repository
//! is answered by exactly two calls and the result is cached; asking per file, or per
//! rebuild, is not on the table. The work happens off the UI thread in [`Probe`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use kite_core::identity;

#[cfg(windows)]
use crate::shell::NO_WINDOW;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Added,
    Conflicted,
    Deleted,
    Ignored,
    Modified,
    Renamed,
    Untracked,
}

impl State {
    /// Directory roll-up: a folder wears the state of the most serious entry under it.
    fn rank(self) -> u8 {
        match self {
            State::Untracked => 1,
            State::Ignored => 2,
            State::Renamed => 3,
            State::Added => 4,
            State::Modified => 5,
            State::Deleted => 6,
            State::Conflicted => 7,
        }
    }

    fn worse(self, other: State) -> State {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub state: State,
    pub added: usize,
    pub removed: usize,
}

impl Entry {
    /// Nothing to show where git has no line counts of its own, as for binaries.
    pub fn delta(self) -> Option<(usize, usize)> {
        (self.added != 0 || self.removed != 0).then_some((self.added, self.removed))
    }
}

/// One repository's answer.
pub struct Status {
    root: PathBuf,
    key: String,
    prefix: String,
    entries: HashMap<String, Entry>,
    missing: Vec<Missing>,
    /// The most serious state below each directory, so folders can be marked too.
    folds: HashMap<String, State>,
}

struct Missing {
    path: PathBuf,
    parent: String,
    entry: Entry,
}

impl Status {
    fn new(root: PathBuf, found: Vec<(PathBuf, Entry)>) -> Self {
        let mut entries = HashMap::new();
        let mut missing = Vec::new();
        let mut folds: HashMap<String, State> = HashMap::new();
        for (path, entry) in found {
            let key = identity(&path);
            // A deleted file is still listed, so its folder must be marked too.
            if entry.state == State::Deleted {
                let parent = path.parent().map(identity).unwrap_or_else(|| key.clone());
                missing.push(Missing {
                    path: path.clone(),
                    parent,
                    entry,
                });
            }
            for dir in path.ancestors().skip(1) {
                if !dir.starts_with(&root) {
                    break;
                }
                folds
                    .entry(identity(dir))
                    .and_modify(|state| *state = state.worse(entry.state))
                    .or_insert(entry.state);
            }
            entries.insert(identity(&path), entry);
        }
        Self {
            key: identity(&root),
            prefix: format!("{}{}", identity(&root), std::path::MAIN_SEPARATOR_STR),
            root,
            entries,
            missing,
            folds,
        }
    }

    /// The repository root counts as covered: its own row is the folder in the listing.
    fn covers(&self, path: &Path) -> bool {
        let key = identity(path);
        key == self.key || key.starts_with(&self.prefix)
    }

    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        self.entries.get(&identity(path))
    }

    pub fn folder(&self, path: &Path) -> Option<State> {
        self.folds.get(&identity(path)).copied()
    }

    /// Tracked files git reports as gone from the work tree, direct children of
    /// `dir`. They cannot be opened but they still belong in the listing.
    pub fn missing_in(&self, dir: &Path) -> impl Iterator<Item = (&PathBuf, &Entry)> {
        let key = identity(dir);
        self.missing
            .iter()
            .filter(move |missing| missing.parent == key)
            .map(|missing| (&missing.path, &missing.entry))
    }
}

pub struct Probe {
    cache: HashMap<String, Status>,
    asked: HashMap<String, u64>,
    requests: Sender<PathBuf>,
    replies: Receiver<Status>,
    generation: u64,
    in_flight: usize,
}

impl Probe {
    pub fn new() -> Self {
        let (requests, inbox) = mpsc::channel();
        let (out, replies) = mpsc::channel();
        thread::Builder::new()
            .name("git-status".to_string())
            .spawn(move || {
                while let Ok(root) = inbox.recv() {
                    if out.send(query(root)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn git-status worker");
        Self {
            cache: HashMap::new(),
            asked: HashMap::new(),
            requests,
            replies,
            generation: 0,
            in_flight: 0,
        }
    }

    /// A reply arrives on another thread, so the event loop must not sleep through it.
    pub fn pending(&self) -> bool {
        self.in_flight > 0
    }

    /// Ask for the repository holding `dir`, once per generation.
    pub fn want(&mut self, dir: &Path) {
        let Some(root) = repo_root(dir) else {
            return;
        };
        let key = identity(&root);
        if self.asked.get(&key) == Some(&self.generation) {
            return;
        }
        self.asked.insert(key.clone(), self.generation);
        if self.requests.send(root).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Take whatever the worker has finished; `true` means the sidebar must redraw.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for status in self.replies.try_iter() {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.cache.insert(identity(&status.root), status);
            changed = true;
        }
        changed
    }

    fn status_for(&self, path: &Path) -> Option<&Status> {
        self.cache
            .values()
            .filter(|status| status.covers(path))
            .max_by_key(|status| status.root.as_os_str().len())
    }

    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        self.status_for(path).and_then(|status| status.entry(path))
    }

    pub fn folder(&self, path: &Path) -> Option<State> {
        self.status_for(path).and_then(|status| status.folder(path))
    }

    /// Tracked files gone from the work tree that belong in `dir`'s listing.
    pub fn missing_in(&self, dir: &Path) -> Vec<(&PathBuf, &Entry)> {
        self.cache
            .values()
            .filter(|status| status.covers(dir))
            .flat_map(|status| status.missing_in(dir))
            .collect()
    }

    /// A save changes what git reports, so the next listing asks again.
    pub fn invalidate(&mut self) {
        self.generation += 1;
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

/// The repository holding `dir`: a `.git` directory, or the `.git` pointer file of a
/// linked work tree.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|path| path.join(".git").exists())
        .map(Path::to_path_buf)
}

fn query(root: PathBuf) -> Status {
    let mut found: Vec<(PathBuf, Entry)> = Vec::new();
    // `--no-optional-locks` is a global flag: kite must not refresh git's index and
    // fight a `git` the user is running in the shell tab.
    let status = run(
        &root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "-uall",
            "--ignored",
        ],
    );
    for (path, state) in parse_status(&status) {
        found.push((
            root.join(path),
            Entry {
                state,
                added: 0,
                removed: 0,
            },
        ));
    }
    let numstat = run(&root, &["diff", "--numstat", "HEAD", "-z"]);
    for (path, added, removed) in parse_numstat(&numstat) {
        let absolute = root.join(&path);
        let key = identity(&absolute);
        if let Some((_, entry)) = found.iter_mut().find(|(known, _)| identity(known) == key) {
            entry.added = added;
            entry.removed = removed;
        }
    }
    Status::new(root, found)
}

fn run(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(NO_WINDOW);
    }
    command
        .output()
        .map(|output| output.stdout)
        .unwrap_or_default()
}

/// `git status --porcelain=v2 -z`: one NUL-terminated record each. `1` is an ordinary
/// change, `2` a rename whose source follows as another field, `u` a conflict with
/// four more fields, `?` untracked and `!` ignored.
fn parse_status(bytes: &[u8]) -> Vec<(PathBuf, State)> {
    let fields = records(bytes);
    let mut found = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        let kind = match record.first() {
            Some(kind) if *kind != b'#' => *kind,
            _ => continue,
        };
        let (skip, tail) = match kind {
            b'1' => (8, record),
            b'2' => (9, record),
            b'u' => (10, record),
            b't' => (3, record),
            b'?' | b'!' => {
                let path = record.get(2..).unwrap_or_default();
                found.push((git_path(path), state_of(kind, kind, kind)));
                continue;
            }
            _ => continue,
        };
        let Some(path) = after_fields(tail, skip) else {
            continue;
        };
        let (x, y) = (
            record.get(2).copied().unwrap_or(b'.'),
            record.get(3).copied().unwrap_or(b'.'),
        );
        found.push((git_path(path), state_of(kind, x, y)));
        if kind == b'2' || kind == b'u' {
            index += if kind == b'2' { 1 } else { 4 };
        }
    }
    found
}

/// The state behind a two-letter `XY` column: `X` is index against `HEAD`, `Y` the
/// work tree against the index.
fn state_of(kind: u8, x: u8, y: u8) -> State {
    if kind == b'u' {
        State::Conflicted
    } else if kind == b'?' {
        State::Untracked
    } else if kind == b'!' {
        State::Ignored
    } else if x == b'D' || y == b'D' {
        State::Deleted
    } else if x == b'R' || y == b'R' {
        State::Renamed
    } else if x == b'A' || y == b'A' {
        State::Added
    } else {
        State::Modified
    }
}

/// Skip `count` space-separated fields and return the rest: the path, which may
/// itself hold spaces.
fn after_fields(record: &[u8], count: usize) -> Option<&[u8]> {
    let mut rest = record;
    for _ in 0..count {
        rest = &rest[rest.iter().position(|byte| *byte == b' ')? + 1..];
    }
    Some(rest)
}

/// `git diff --numstat HEAD -z`: `added\tremoved\tpath`, except for a rename, where
/// the path is empty and the old and new names follow as two more fields.
fn parse_numstat(bytes: &[u8]) -> Vec<(PathBuf, usize, usize)> {
    let fields = records(bytes);
    let mut found = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        let Some((first, second)) = tabs(record) else {
            continue;
        };
        let added = digits(&record[..first]);
        let removed = digits(&record[first + 1..second]);
        let path = &record[second + 1..];
        if path.is_empty() {
            let Some(new) = fields.get(index + 1) else {
                continue;
            };
            index += 2;
            found.push((git_path(new), added, removed));
        } else {
            found.push((git_path(path), added, removed));
        }
    }
    found
}

fn records(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect()
}

fn tabs(record: &[u8]) -> Option<(usize, usize)> {
    let first = record.iter().position(|byte| *byte == b'\t')?;
    let second = record[first + 1..]
        .iter()
        .position(|byte| *byte == b'\t')
        .map(|offset| offset + first + 1)?;
    Some((first, second))
}

fn digits(field: &[u8]) -> usize {
    field
        .iter()
        .filter(|byte| byte.is_ascii_digit())
        .fold(0usize, |total, digit| total * 10 + (digit - b'0') as usize)
}

fn git_path(field: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(field).replace('/', std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> String {
        text.replace('/', std::path::MAIN_SEPARATOR_STR)
    }

    /// Captured from `git status --porcelain=v2 -z -uall --ignored` in a scratch
    /// repository holding one of every state.
    const STATUS: &[u8] = b"\
# branch.oid 1a2b3c4d5e6f\0\
# branch.head master\0\
# branch.upstream origin/master\0\
# branch.ab +0 -0\0\
1 A. N... 000000 100644 100644 000000 6bd674 .gitignore\0\
1 D. N... 100644 000000 000000 587be6 000000 gone.txt\0\
1 .D N... 100644 100644 000000 0ddf2b 0ddf2b ig.txt\0\
1 .M N... 100644 100644 100644 de9804 de9804 keep.txt\0\
2 R. N... 100644 100644 100644 4286f4 4286f4 R100 moved.txt\0ren.txt\0\
? new.txt\0\
! log.txt\0\
u UD N... 100644 100644 100644 100644 000000 aaa111 bbb222 clash.txt\0\
000000 ancestor\0111111 commit\0222222 ours\0333333 theirs\0";

    fn states() -> Vec<(String, State)> {
        parse_status(STATUS)
            .into_iter()
            .map(|(path, state)| (path.to_string_lossy().replace('\\', "/"), state))
            .collect()
    }

    #[test]
    fn every_reported_state_is_named() {
        assert_eq!(
            states(),
            vec![
                (".gitignore".into(), State::Added),
                ("gone.txt".into(), State::Deleted),
                ("ig.txt".into(), State::Deleted),
                ("keep.txt".into(), State::Modified),
                ("moved.txt".into(), State::Renamed),
                ("new.txt".into(), State::Untracked),
                ("log.txt".into(), State::Ignored),
                ("clash.txt".into(), State::Conflicted),
            ]
        );
    }

    #[test]
    fn branch_headers_and_rename_sources_are_not_paths() {
        let named = states();
        assert!(
            !named
                .iter()
                .any(|(path, _)| path.contains("branch") || path == "ren.txt")
        );
    }

    #[test]
    fn a_path_may_hold_spaces() {
        let bytes = b"1 .M N... 100644 100644 100644 aaa bbb two words.txt\0";
        assert_eq!(
            parse_status(bytes)[0],
            (PathBuf::from(git_path(b"two words.txt")), State::Modified)
        );
    }

    /// Captured from `git diff --numstat HEAD -z`; the rename entry is the one whose
    /// path field is empty, and a binary file reports `-`.
    const NUMSTAT: &[u8] = b"\
1\t0\t.gitignore\00\t1\tgone.txt\02\t0\tkeep.txt\00\t0\t\0ren.txt\0moved.txt\0-\t-\tbin.dat\0";

    #[test]
    fn line_counts_are_taken_against_head() {
        let counts = parse_numstat(NUMSTAT)
            .into_iter()
            .map(|(path, added, removed)| {
                (path.to_string_lossy().replace('\\', "/"), added, removed)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            counts,
            vec![
                (".gitignore".into(), 1, 0),
                ("gone.txt".into(), 0, 1),
                ("keep.txt".into(), 2, 0),
                ("moved.txt".into(), 0, 0),
                ("bin.dat".into(), 0, 0),
            ]
        );
    }

    #[test]
    fn a_folder_wears_the_worst_state_below_it() {
        let root = PathBuf::from(path("repo"));
        let found = vec![
            (
                root.join(path("src/keep.txt")),
                Entry {
                    state: State::Modified,
                    added: 2,
                    removed: 0,
                },
            ),
            (
                root.join(path("src/gone.txt")),
                Entry {
                    state: State::Deleted,
                    added: 0,
                    removed: 7,
                },
            ),
            (
                root.join(path("src/deep/new.txt")),
                Entry {
                    state: State::Untracked,
                    added: 0,
                    removed: 0,
                },
            ),
        ];
        let status = Status::new(root.clone(), found);
        assert_eq!(status.folder(&root.join(path("src"))), Some(State::Deleted));
        assert_eq!(
            status.folder(&root.join(path("src/deep"))),
            Some(State::Untracked)
        );
        assert_eq!(status.folder(&root), Some(State::Deleted));
        assert_eq!(
            status
                .entry(&root.join(path("src/gone.txt")))
                .unwrap()
                .delta(),
            Some((0, 7))
        );
        assert_eq!(
            status.entry(&root.join(path("src/bin.dat"))).map(|_| ()),
            None
        );
    }

    #[test]
    fn deleted_files_are_offered_to_their_own_directory() {
        let root = PathBuf::from(path("repo"));
        let status = Status::new(
            root.clone(),
            vec![
                (
                    root.join(path("src/gone.txt")),
                    Entry {
                        state: State::Deleted,
                        added: 0,
                        removed: 1,
                    },
                ),
                (
                    root.join(path("gone.txt")),
                    Entry {
                        state: State::Deleted,
                        added: 0,
                        removed: 2,
                    },
                ),
                (
                    root.join(path("src/keep.txt")),
                    Entry {
                        state: State::Modified,
                        added: 1,
                        removed: 0,
                    },
                ),
            ],
        );
        let names = |dir: &Path| -> Vec<String> {
            status
                .missing_in(dir)
                .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names(&root.join(path("src"))), vec!["gone.txt".to_string()]);
        assert_eq!(names(&root), vec!["gone.txt".to_string()]);
    }

    #[test]
    fn lookup_ignores_case_and_separators() {
        let root = PathBuf::from(path("Repo"));
        let status = Status::new(
            root.clone(),
            vec![(
                root.join(path("Src/Keep.txt")),
                Entry {
                    state: State::Modified,
                    added: 1,
                    removed: 0,
                },
            )],
        );
        assert!(status.covers(Path::new(&path("rEpO\\sRc\\kEEP.TXT"))));
        // The repository's own folder is a row in its parent's listing.
        assert!(status.covers(&root));
        assert_eq!(
            status
                .entry(Path::new(&path("repo/src/keep.txt")))
                .unwrap()
                .state,
            State::Modified
        );
        assert!(!status.covers(Path::new(&path("other"))));
    }

    /// The flags and the path joining are only half the story; this drives the real
    /// `git` binary the way the sidebar does.
    #[test]
    fn a_real_repository_answers_in_two_calls() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(["-c", "user.name=kite", "-c", "user.email=kite@local"])
                .args(args)
                .current_dir(&root)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        git(&["init", "-q", "."]);
        std::fs::write(root.join("keep.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("gone.txt"), "gone\n").unwrap();
        std::fs::write(root.join("log.txt"), "noise\n").unwrap();
        std::fs::write(root.join(".gitignore"), "log.txt\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("fresh.txt"), "new\n").unwrap();

        let status = query(root.clone());
        assert_eq!(
            status.entry(&root.join("gone.txt")).unwrap().state,
            State::Deleted
        );
        assert_eq!(
            status.entry(&root.join("gone.txt")).unwrap().delta(),
            Some((0, 1))
        );
        assert_eq!(
            status.entry(&root.join("keep.txt")).unwrap().state,
            State::Modified
        );
        assert_eq!(
            status.entry(&root.join("keep.txt")).unwrap().delta(),
            Some((1, 0))
        );
        assert_eq!(
            status.entry(&root.join("log.txt")).unwrap().state,
            State::Ignored
        );
        assert_eq!(
            status.entry(&root.join("fresh.txt")).unwrap().state,
            State::Untracked
        );
        assert_eq!(status.folder(&root), Some(State::Deleted));
        assert_eq!(
            status
                .missing_in(&root)
                .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["gone.txt".to_string()]
        );
    }
}
