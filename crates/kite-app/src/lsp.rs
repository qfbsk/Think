//! Asking a language server what comes next.
//!
//! This is the whole of kite's completion now. The engine it replaced — ctags
//! over the project, a keyword table per language, the words already in the
//! buffer — is gone, because all three were guesses about what a name means. A
//! language server has read the program: it knows the type of the thing behind
//! the dot, which headers are really reachable, and of the thousand names in the
//! project which one this expression wants.
//!
//! Two rules shape everything below.
//!
//! The typing thread never waits. A server answers in three milliseconds or in
//! three seconds, and neither belongs between a keypress and the screen, so
//! `ask` only posts the question and the answer is picked up later by the same
//! poll the sidebar and the band already use. Nothing here may block on a pipe
//! either — a server that is busy indexing stops reading, and a full pipe would
//! otherwise hand that stall to whoever typed last. So every server owns a
//! writer thread and a reader thread, and the user interface only ever touches
//! a mutex over a small map.
//!
//! And a language with no server gets no completion. There is deliberately no
//! fallback: a guess dressed up as an answer teaches the reader that the faint
//! text means nothing, which costs more than the silence.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Value};

use crate::complete::{Call, ExtraEdit, Item, Kind};
use crate::shell::NO_WINDOW;

/// Above this many bytes a page is not offered to a server at all. Full-text
/// sync of a monster costs more than the answer is worth, and the files that
/// reach the ceiling are being read, not typed into.
const MAX_SYNC_BYTES: usize = 4 * 1024 * 1024;

/// How many answers to keep. A server will happily return four thousand names
/// for a single letter, and the panel shows eleven.
const POOL: usize = 200;

/// Answers waiting to be read out. Every keystroke posts a question, and the
/// reader only ever wants the newest answer for the place the caret has reached,
/// so older ones are thrown away rather than left to pile up in the map.
const WAITING: u64 = 32;

/// The initialize reply is recognised by its id, so that `initialized` can be
/// sent the moment it lands rather than on a timer.
const INIT: u64 = 0;
/// Kept clear of the request counter so a shutdown reply is never mistaken for
/// the handshake one.
const SHUTDOWN: u64 = u64::MAX;

/// A page as a server sees it: which file, in which language, saying what.
///
/// This is not part of asking a question. A server that is only told what the
/// page says when it is also asked something sees the page as it did at the last
/// question, and every report it volunteers after that is about text the reader
/// has since changed.
pub struct Page<'a> {
    pub path: &'a Path,
    pub language: &'static str,
    /// The whole document: a full-text sync has to hand all of it over.
    pub text: &'a str,
    pub revision: u64,
}

/// Where a question is asked from, on top of the page it is asked about.
pub struct Asking<'a> {
    /// The line the caret stands on, which is what a server's counting of
    /// columns has to be turned back against.
    pub caret_line: &'a str,
    pub line: usize,
    /// The caret's character index on that line, in kite's counting.
    pub column: usize,
    /// The character behind the caret, if there is one. A server names the
    /// characters that mean "ask me here" — a dot, an include bracket — and
    /// after one of those its list is scoped to the context. After any other
    /// keystroke the list is the whole visible program, which is not an answer
    /// to anything.
    pub trigger: Option<char>,
}

/// An answer, with its range already moved into kite's counting.
pub struct Answer {
    pub items: Vec<Item>,
    /// Where the replacement starts and ends on the caret's line. One range for
    /// the whole list: a server picks it from the position it was handed, so
    /// every item offered at one caret replaces the same span. The end can sit
    /// past the caret — completing the `strn` of `strncpy` with the caret in the
    /// middle of the word is a real thing to do, and writing only up to the
    /// caret would leave the rest of the old name behind.
    pub start: usize,
    pub end: usize,
    /// The server said it stopped short of the whole answer. It is the only
    /// reason to ask again after the next letter: a list that was given in full
    /// needs no second question, and a client that asks anyway is the reason a
    /// completion arrives a word late.
    pub incomplete: bool,
}

struct Server {
    out: Sender<Value>,
    /// Revision per document, so moving the caret resends nothing and a file
    /// the server has never seen is opened rather than changed.
    documents: HashMap<PathBuf, i64>,
    /// The characters this server asked to be consulted on, as it declared
    /// them in the handshake. Empty until the reply lands, which is why the
    /// first keypress on a cold server gets no ghost from punctuation.
    triggers: Arc<Mutex<Vec<String>>>,
    alive: Arc<AtomicBool>,
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        // The writer thread ends when `out` goes with it, which closes stdin
        // and ends most servers politely. The kill is for the ones that do not.
        let _ = self.child.kill();
    }
}

/// Which folder a server gets to call home. It is the ancestor that declares
/// itself a project, because that is where the manifest, the build file and the
/// dependency lock live, and a server started below one of those reads only
/// half a program.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    root: PathBuf,
    language: &'static str,
}

#[derive(Default)]
struct State {
    servers: HashMap<Key, Server>,
    next_id: u64,
}

/// One thing a server found wrong with a page, in the server's own coordinates:
/// lines as shown and columns in UTF-16 units. It is turned into document
/// positions only where the page's own text is known, which is the same rule a
/// completion range follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDiagnostic {
    pub start: (usize, u64),
    pub end: (usize, u64),
    /// Whether this is the kind of thing that stops the program compiling. The
    /// protocol numbers four severities; the page has two colours, and an
    /// `Info` wearing red would teach the reader to ignore the red.
    pub error: bool,
    pub message: String,
    /// Who said it, and what it called the trouble: `clang`,
    /// `-Wimplicit-function-declaration`. A card with no attribution on it is a
    /// claim, and the reader cannot weigh a claim.
    pub note: String,
}

pub struct Servers {
    state: Arc<Mutex<State>>,
    answers: Arc<Mutex<HashMap<u64, Value>>>,
    /// What the servers have said about each page, filed under [`page_key`]. A
    /// server publishes these whenever it feels like it, which is the only
    /// reason the interface has a generation counter on it: the screen has to
    /// know when to look.
    diagnostics: Arc<Mutex<HashMap<String, Vec<RawDiagnostic>>>>,
    generation: Arc<AtomicU64>,
}

impl Default for Servers {
    fn default() -> Self {
        Self::new()
    }
}

impl Servers {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                servers: HashMap::new(),
                next_id: INIT + 1,
            })),
            answers: Arc::new(Mutex::new(HashMap::new())),
            diagnostics: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// What the server last said about this page. An empty list is an answer
    /// too: it is how a file that has just become clean takes its red off the
    /// screen.
    pub fn diagnostics(&self, path: &Path) -> Vec<RawDiagnostic> {
        self.diagnostics
            .lock()
            .unwrap()
            .get(&page_key(path))
            .cloned()
            .unwrap_or_default()
    }

    /// Bumped every time a server tells us something about a page. The screen
    /// cannot listen for the notifications, so it asks this instead.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Whether a server already holds this page at this revision. The screen asks
    /// once per frame before it copies anything, and the answer is the server's
    /// own list rather than a note the view keeps: a page whose server died, or
    /// whose tab was closed and opened again, has to be offered again, and a view
    /// that remembers posting it would never do so.
    pub fn holds(&self, path: &Path, language: &'static str, revision: u64) -> bool {
        let version = revision.max(1) as i64;
        let state = self.state.lock().unwrap();
        state.servers.iter().any(|(key, server)| {
            key.language == language
                && server.alive.load(Ordering::Relaxed)
                && server.documents.get(path).is_some_and(|seen| *seen == version)
        })
    }

    /// Put a page in front of its server: start the server if this is the first
    /// page of its project, open the file if it has never seen it, and post the
    /// words if they are not the ones it was last told. Nothing is asked, so
    /// there is nothing to wait for — what comes back is whatever the server
    /// decides to say about the page, which is the report under the text.
    pub fn sync(&self, page: &Page) {
        let _ = self.tell(page);
    }

    /// The page as the server now has it, under the name it knows it by. Nothing
    /// means no server for this language, a page too big to sync, or a server
    /// that died and has not been replaced yet.
    fn tell(&self, page: &Page) -> Option<String> {
        if page.text.len() > MAX_SYNC_BYTES {
            return None;
        }
        let key = Key {
            root: project_root(page.path),
            language: page.language,
        };
        self.start(&key);
        let mut state = self.state.lock().unwrap();
        let server = state.servers.get_mut(&key)?;
        if !server.alive.load(Ordering::Relaxed) {
            return None;
        }
        let at = uri(page.path);
        let version = page.revision.max(1) as i64;
        match server.documents.get(page.path).copied() {
            None => {
                server.documents.insert(page.path.to_path_buf(), version);
                let _ = server.out.send(json!({
                    "method": "textDocument/didOpen",
                    "params": {"textDocument": {
                        "uri": at, "languageId": page.language,
                        "version": version, "text": page.text}}}));
            }
            Some(seen) if seen != version => {
                server.documents.insert(page.path.to_path_buf(), version);
                let _ = server.out.send(json!({
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {"uri": at, "version": version},
                        "contentChanges": [{"text": page.text}]}}));
            }
            Some(_) => {}
        }
        Some(at)
    }

    /// Post the question and take back the id to look the answer up with. The
    /// page goes over first, because a question about a text the server has
    /// never seen is answered against the wrong file.
    pub fn ask(&self, page: &Page, asking: &Asking) -> Option<u64> {
        let at = self.tell(page)?;
        let key = Key {
            root: project_root(page.path),
            language: page.language,
        };
        let id = {
            let mut state = self.state.lock().unwrap();
            let id = state.next_id;
            state.next_id += 1;
            id
        };
        let state = self.state.lock().unwrap();
        let server = state.servers.get(&key)?;
        // The spec asks in three ways: the reader asked on purpose (1),
        // typed a character the server named as a trigger (2), or typed
        // anything at all (3, not used here). Only typing asks here, and a
        // named trigger character is what makes it question 2 — read off
        // the handshake rather than guessed from the character.
        let triggers = server.triggers.lock().unwrap().clone();
        let context = match asking.trigger {
            Some(ch) if declared(&triggers, Some(ch)) => {
                json!({"triggerKind": 2, "triggerCharacter": ch.to_string()})
            }
            _ => json!({"triggerKind": 1}),
        };
        let _ = server.out.send(json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": at},
                "position": {
                    "line": asking.line,
                    "character": utf16_column(asking.caret_line, asking.column)},
                "context": context}}));
        Some(id)
    }

    /// Take the answer if it has arrived. `caret_line` is the text of the line
    /// the caret is on, needed to turn the server's columns back into kite's.
    pub fn take(&self, id: u64, caret_line: &str, column: usize) -> Option<Answer> {
        let raw = {
            let mut answers = self.answers.lock().unwrap();
            answers.remove(&id)
        }?;
        Some(answer(&raw, caret_line, column))
    }

    /// Whether the server for this page named the character behind the caret as
    /// one it wants to be asked about. It is the server's own list, not a guess
    /// at what punctuation means: clangd names `. < > : " / *` and not `(`, and
    /// that difference is the whole reason a ghost after `scanf(` used to offer
    /// `size_t` — the list at that caret is the entire standard library.
    pub fn declares(&self, path: &Path, language: &'static str, behind: Option<char>) -> bool {
        let key = Key {
            root: project_root(path),
            language,
        };
        let state = self.state.lock().unwrap();
        let Some(server) = state.servers.get(&key) else {
            return false;
        };
        let triggers = server.triggers.lock().unwrap();
        declared(&triggers, behind)
    }

    /// Ask on behalf of a server that the machine running the test may not have.
    /// The id is handed out exactly as `ask` hands one out, so the reading side
    /// of the contract — positions, ranges, the poll — is the real code.
    #[cfg(test)]
    pub(crate) fn fake_question(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        id
    }

    /// The reply a real server would have put on the pipe.
    #[cfg(test)]
    pub(crate) fn fake_answer(&self, id: u64, raw: Value) {
        self.answers.lock().unwrap().insert(id, raw);
    }

    /// A page closed. The buffer is handed back; the server itself goes when
    /// nothing else of its project is open, because a language server is
    /// hundreds of megabytes of somebody's index and should not outlive the tab
    /// that asked for it.
    pub fn forget(&self, path: &Path, language: &'static str, still_open: bool) {
        let mut state = self.state.lock().unwrap();
        let key = Key {
            root: project_root(path),
            language,
        };
        let Some(server) = state.servers.get_mut(&key) else {
            return;
        };
        server.documents.remove(path);
        // A page the server no longer has cannot be saying anything about it,
        // and its last report would otherwise stay painted on a file that is
        // closed and on the one opened in its place.
        self.diagnostics.lock().unwrap().remove(&page_key(path));
        self.generation.fetch_add(1, Ordering::Relaxed);
        let _ = server
            .out
            .send(json!({"method":"textDocument/didClose","params":{
                "textDocument": {"uri": uri(path)}}}));
        if !still_open {
            state.servers.remove(&key);
        }
    }

    /// Everything, because the app is closing.
    pub fn quit(&self) {
        let mut state = self.state.lock().unwrap();
        for server in state.servers.values_mut() {
            let _ = server
                .out
                .send(json!({"id":SHUTDOWN,"method":"shutdown"}));
            let _ = server.out.send(json!({"method":"exit"}));
            let _ = server.child.kill();
        }
        state.servers.clear();
    }

    /// Bring the server for one project and one language up, if it is not
    /// already running. A dead one is dropped and replaced rather than reused,
    /// because its pipes are gone.
    fn start(&self, key: &Key) {
        let mut state = self.state.lock().unwrap();
        if let Some(server) = state.servers.get(key) {
            if server.alive.load(Ordering::Relaxed) {
                return;
            }
            state.servers.remove(key);
        }
        let Some((program, args)) = launch(key.language) else {
            return;
        };
        let Some(server) = spawn(
            key,
            &program,
            &args,
            self.answers.clone(),
            self.diagnostics.clone(),
            self.generation.clone(),
        ) else {
            return;
        };
        state.servers.insert(key.clone(), server);
    }
}

/// Whether one character is in a server's declared trigger list. The list holds
/// strings because the protocol allows sequences; this client only ever holds
/// the single character behind the caret, so only single-character triggers can
/// match.
fn declared(triggers: &[String], behind: Option<char>) -> bool {
    let Some(ch) = behind else {
        return false;
    };
    triggers.iter().any(|trigger| {
        let mut chars = trigger.chars();
        chars.next() == Some(ch) && chars.next().is_none()
    })
}

/// The delimiter the include the completion span sits in was opened with: a
/// quote for `#include "..."`, an angle bracket for `#include <...>`. The span
/// starts just inside it, so the last thing before it is the opener.
fn include_opener(line: &str, start: usize) -> Option<char> {
    match line.chars().take(start).collect::<String>().trim_end().chars().last() {
        Some(ch @ ('"' | '<')) => Some(ch),
        _ => None,
    }
}

/// The delimiter that closes the include the span sits in: a quote answers a
/// quote, an angle bracket answers a bracket.
fn closing_delimiter(line: &str, start: usize) -> Option<char> {
    match include_opener(line, start) {
        Some('"') => Some('"'),
        Some('<') => Some('>'),
        _ => None,
    }
}

/// The characters a server named in its handshake as ones it wants to be asked
/// on. Missing or malformed means none, and then only a typed word earns a
/// ghost.
fn named_triggers(reply: &Value) -> Vec<String> {
    reply
        .pointer("/result/capabilities/completionProvider/triggerCharacters")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What to run for a language, and what to say to it. No language outside this
/// list gets a completion, on purpose: a list of the words already in the file
/// is what kite used to do, and it was worse than nothing.
fn launch(language: &str) -> Option<(PathBuf, Vec<String>)> {
    match language {
        "rust" => Some((locate("rust-analyzer")?, Vec::new())),
        // stdio is clangd's default and the flag itself is gone in 23, so the
        // argument list carries opinions rather than transport.
        // `iwyu` because the name a reader picks is usually a name whose header
        // the file has not included yet, and taking it without the include is
        // taking code that will not compile: the edit is attached to the item
        // and only made when the item is, never on the ghost.
        "c" | "cpp" => Some((
            locate("clangd")?,
            vec![
                "--header-insertion=iwyu".to_string(),
                "--limit-results=200".to_string(),
            ],
        )),
        "python" => {
            // pyright arrives as an npm bin, which on Windows is a `.cmd` that
            // starts node on a JavaScript file. Windows will not start the shim
            // itself, so the shim is read and what it names is run: one process
            // instead of two, and the one `kill` can reach.
            let shim = locate_npm("pyright-langserver")?;
            let script = npm_script(&shim)?;
            let node = shim
                .parent()
                .map(|dir| dir.join("node.exe"))
                .filter(|node| node.is_file())
                .or_else(|| locate("node"))?;
            Some((
                node,
                vec![script.to_string_lossy().into_owned(), "--stdio".to_string()],
            ))
        }
        _ => None,
    }
}

/// The file an npm bin really runs. Every one of them is laid out the same way:
/// a quoted path into `node_modules` ending in `.js`, prefixed by `%dp0%`.
fn npm_script(shim: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(shim).ok()?;
    let dir = shim.parent()?;
    for token in text.split('"') {
        let token = token.trim();
        let Some(rest) = token.strip_prefix("%dp0%") else {
            continue;
        };
        if token.ends_with(".js") && token.contains("node_modules") {
            let rest = rest.trim_start_matches(['\\', '/']);
            return Some(dir.join(rest.replace('\\', "/")));
        }
    }
    None
}

/// Find the server. PATH first, since that is where rust-analyzer and the npm
/// shim already are, then the folder clangd's own installers use so that the
/// one thing not on PATH is still found without being told where it is.
fn locate(program: &str) -> Option<PathBuf> {
    search(&[format!("{program}.exe"), program.to_string()])
}

/// The same search, admitting npm's `.cmd` shims.
fn locate_npm(program: &str) -> Option<PathBuf> {
    search(&[
        format!("{program}.cmd"),
        format!("{program}.exe"),
        program.to_string(),
    ])
}

fn search(names: &[String]) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::var("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    for dir in dirs.into_iter().chain(extra_dirs()) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The numbers inside a folder name, in order. `%LOCALAPPDATA%\clangd` can hold
/// more than one installed version, and a string sort puts 9 above 23; compared
/// as the numbers they are, it does not.
fn version_parts(name: &std::ffi::OsStr) -> Vec<u64> {
    name.to_string_lossy()
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

/// `%LOCALAPPDATA%\clangd\**\bin`, newest version first. This is the folder
/// clangd's own archive unpacks into, and it is on nobody's PATH, so finding it
/// here is the difference between C working out of the box and being told to
/// configure something.
fn extra_dirs() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    let root = local.join("clangd");
    let mut dirs = Vec::new();
    for base in [root.clone(), root.join("clangd")] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let bin = entry.path().join("bin");
            if bin.is_dir() && !dirs.contains(&bin) {
                dirs.push(bin);
            }
        }
    }
    let version = |path: &Path| -> Vec<u64> {
        path.parent()
            .and_then(|p| p.file_name())
            .map(version_parts)
            .unwrap_or_default()
    };
    dirs.sort_by(|a, b| version(b).cmp(&version(a)));
    dirs
}

fn spawn(
    key: &Key,
    program: &Path,
    args: &[String],
    answers: Arc<Mutex<HashMap<u64, Value>>>,
    told: Arc<Mutex<HashMap<String, Vec<RawDiagnostic>>>>,
    bumped: Arc<AtomicU64>,
) -> Option<Server> {
    // Whatever a server complains about goes where it can be read after the
    // fact; a windowed app has no console to put it in.
    let log = std::env::temp_dir().join(format!("kite-lsp-{}.log", key.language));
    let errors = std::fs::File::create(&log).ok().map(Stdio::from);
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&key.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(errors.unwrap_or_else(Stdio::null));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;

    let (out, inbox) = channel::<Value>();
    let alive = Arc::new(AtomicBool::new(true));
    let triggers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    thread::spawn(move || {
        let mut stdin = stdin;
        // A document opened before the server has answered `initialized` is a
        // protocol error, and a server that dislikes it ignores the document
        // rather than saying so — after which every completion on that file is
        // empty and nothing on the wire says why. So the messages a keystroke
        // produced wait here, in order, until the handshake is over.
        let mut held: Vec<Value> = Vec::new();
        let mut running = false;
        let write = |stdin: &mut ChildStdin, message: &Value| write_frame(stdin, message).is_ok();
        for mut message in inbox {
            stamp(&mut message);
            match message.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    if !write(&mut stdin, &message) {
                        break;
                    }
                }
                Some("initialized") => {
                    running = true;
                    if !write(&mut stdin, &message) {
                        break;
                    }
                    if !held.drain(..).all(|waiting| write(&mut stdin, &waiting)) {
                        break;
                    }
                }
                _ if running => {
                    if !write(&mut stdin, &message) {
                        break;
                    }
                }
                _ => held.push(message),
            }
        }
    });

    thread::spawn({
        let listening = alive.clone();
        let speaking = out.clone();
        let declared = triggers.clone();
        move || {
            listen(
                BufReader::new(stdout),
                answers,
                speaking,
                declared,
                listening,
                told,
                bumped,
            )
        }
    });

    let _ = out.send(initialize(&key.root));
    Some(Server {
        out,
        documents: HashMap::new(),
        triggers,
        alive,
        child,
    })
}

fn initialize(root: &Path) -> Value {
    json!({
        "id": INIT, "method": "initialize",
        "params": {
            "processId": std::process::id() as i64,
            "clientInfo": {"name": "kite", "version": env!("CARGO_PKG_VERSION")},
            "locale": "en",
            "rootUri": uri(root),
            "rootPath": root.to_string_lossy(),
            "capabilities": {
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "didChangeConfiguration": {"dynamicRegistration": false},
                    "executeCommand": {}
                },
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false},
                    "completion": {
                        "dynamicRegistration": false,
                        "contextSupport": true,
                        "completionItem": {
                            "snippetSupport": true,
                            "documentationFormat": ["plaintext", "markdown"],
                            "resolveSupport": {"properties": ["detail", "documentation"]},
                            "insertTextMode": 2,
                            "tagSupport": {"valueSet": [1]}
                        },
                        "completionItemKind": {"valueSet": [
                            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
                            20, 21]}
                    },
                    "signatureHelp": {
                        "dynamicRegistration": false,
                        "signatureInformation": {"documentationFormat": ["plaintext"]}
                    },
                    "hover": {"dynamicRegistration": false, "contentFormat": ["plaintext"]},
                    "publishDiagnostics": {
                        "relatedInformation": false,
                        // The tag is the only general way to tell "this text is
                        // not needed" apart from "this program is wrong", and a
                        // server may withhold tags it was never told are read.
                        "tagSupport": {"valueSet": [1, 2]}
                    }
                },
                "window": {"workDoneProgress": false, "showMessage": {}}
            },
            "initializationOptions": Value::Null,
            "trace": "off"
        }})
}

/// The version belongs to the envelope, not to whoever built the message.
/// clangd 23 reads it before anything else and throws the message away without
/// it — a notification is exactly the kind of message someone forgets it on,
/// and a lost didOpen is a completion list that never arrives.
fn stamp(message: &mut Value) {
    if let Some(object) = message.as_object_mut() {
        object
            .entry("jsonrpc")
            .or_insert_with(|| Value::String("2.0".into()));
    }
}

/// One `Content-Length` framed message.
fn write_frame(stdin: &mut ChildStdin, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).unwrap_or_default();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()
}

fn read_frame(reader: &mut BufReader<std::process::ChildStdout>) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Everything the server says, for as long as it says it.
///
/// Three kinds of message share the stream and only one of them is an answer:
/// a reply carries an id and no method, a request from the server carries both,
/// and everything else is a notification. The one worth keeping is
/// `textDocument/publishDiagnostics` — the server's own opinion of the page,
/// which it sends whenever it re-reads it — and it is kept per page with the
/// generation bumped, because a screen that only looks when it types would show
/// yesterday's red lines.
fn listen(
    mut reader: BufReader<std::process::ChildStdout>,
    answers: Arc<Mutex<HashMap<u64, Value>>>,
    speak: Sender<Value>,
    triggers: Arc<Mutex<Vec<String>>>,
    alive: Arc<AtomicBool>,
    told: Arc<Mutex<HashMap<String, Vec<RawDiagnostic>>>>,
    bumped: Arc<AtomicU64>,
) {
    while let Some(message) = read_frame(&mut reader) {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if method == "textDocument/publishDiagnostics" {
                let Some(params) = message.get("params") else { continue };
                let Some(at) = params.get("uri").and_then(Value::as_str) else {
                    continue;
                };
                // A server answers for whichever file it resolved that URI to,
                // and the spelling it settled on is not always the one kite sent.
                let Some(owner) = path_from_uri(at) else { continue };
                let found = params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(raw_diagnostic).collect())
                    .unwrap_or_default();
                told.lock().unwrap().insert(page_key(&owner), found);
                bumped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Only a message carrying an id is the server asking something.
            // Diagnostics are the server telling, and a reply to telling is a
            // protocol error: clangd logs it and a stricter server may not
            // forgive it.
            let Some(call) = message.get("id") else { continue };
            // A server that cannot get an answer to its own request tends to
            // wait for it, and rust-analyzer asks before it will configure
            // itself. Null means "no, and I have no opinion", which is the
            // truth here.
            let result = match method {
                "workspace/configuration" => match message.pointer("/params/items") {
                    Some(Value::Array(items)) => Value::Array(vec![Value::Null; items.len()]),
                    _ => Value::Null,
                },
                _ => Value::Null,
            };
            let _ = speak.send(json!({"id": call, "result": result}));
            continue;
        }
        let Some(id) = message.get("id").and_then(Value::as_u64) else { continue };
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        if id == INIT {
            // The list of characters the server wants to be asked on is part of
            // the handshake and never mentioned again. It is what tells a dot
            // apart from an opening bracket, so it is kept rather than dropped
            // with the rest of the capabilities.
            *triggers.lock().unwrap() = named_triggers(&message);
            let _ = speak.send(json!({"method": "initialized", "params": {}}));
            continue;
        }
        let mut waiting = answers.lock().unwrap();
        waiting.insert(id, result);
        // Ids only ever grow, so anything this far behind the newest answer was
        // asked for somewhere the caret has since left.
        if waiting.len() as u64 > WAITING {
            waiting.retain(|seen, _| id.saturating_sub(*seen) < WAITING);
        }
    }
    alive.store(false, Ordering::Relaxed);
}

/// `DiagnosticTag::Unnecessary`, the protocol's own label for "this text is not
/// needed here". `Deprecated` (2) is deliberately not dropped: a program built on
/// something that is going away is a different claim from one that never used the
/// line at all.
const UNNECESSARY: u64 = 1;

/// One `publishDiagnostics` entry, kept only if it is worth a line on the page.
/// The protocol numbers severities Error, Warning, Information, Hint and leaves
/// the field out when the server has no opinion; the two that stop or doubt the
/// program are kept, and an `Info` wearing the same red as an error is how the
/// red stops meaning anything.
fn raw_diagnostic(raw: &Value) -> Option<RawDiagnostic> {
    let range = raw.get("range")?;
    let at = |side: &str| {
        let side = range.get(side)?;
        Some((
            side.get("line").and_then(Value::as_u64)? as usize,
            side.get("character").and_then(Value::as_u64)?,
        ))
    };
    let message = raw.get("message").and_then(Value::as_str).unwrap_or("");
    if message.is_empty() {
        return None;
    }
    let severity = raw.get("severity").and_then(Value::as_u64).unwrap_or(1);
    if severity > 2 {
        return None;
    }
    // The protocol itself marks the reports whose whole point is that some text
    // here is not needed — an include nothing names, a branch nothing reaches —
    // and every server uses that marker for its own language. Such a report is
    // not a trouble in the program: the program does exactly the same with it and
    // without it. kite draws what is wrong and not what could be shorter, and it
    // decides that on the tag rather than on a list of header names, because a
    // list is a patch that has to be redone for the next header.
    if raw
        .get("tags")
        .and_then(Value::as_array)
        .is_some_and(|tags| tags.iter().any(|tag| tag.as_u64() == Some(UNNECESSARY)))
    {
        return None;
    }
    let mut note = raw
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let code = match raw.get("code") {
        Some(Value::String(code)) => code.clone(),
        Some(Value::Number(code)) => code.to_string(),
        _ => String::new(),
    };
    if !code.is_empty() {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(&code);
    }
    Some(RawDiagnostic {
        start: at("start")?,
        end: at("end")?,
        error: severity == 1,
        message: message.to_string(),
        note,
    })
}

/// The names, and the span they replace.
fn answer(raw: &Value, caret_line: &str, column: usize) -> Answer {
    let items = match raw {
        Value::Array(items) => items.clone(),
        other => other.get("items").and_then(Value::as_array).cloned().unwrap_or_default(),
    };
    let mut found: Vec<Item> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut start = column;
    let mut end = column;
    let mut ranged = false;
    for item in items.iter() {
        let label = item.get("label").and_then(Value::as_str).unwrap_or("");
        let edit = item.get("textEdit");
        let raw = edit
            .and_then(|e| e.get("newText"))
            .and_then(Value::as_str)
            .or_else(|| item.get("insertText").and_then(Value::as_str))
            .unwrap_or(label);
        let (name, text, call) = if item.get("insertTextFormat").and_then(Value::as_i64) == Some(2) {
            let (unrolled, stops) = snippet(raw);
            // The row reads as the name, not as the call: `printf`, with the
            // parentheses and the placeholders waiting for the accept.
            (listed(label, &unrolled), unrolled.clone(), Some(Call { text: unrolled, stops }))
        } else {
            (listed(label, raw), raw.to_string(), None)
        };
        if text.is_empty() {
            continue;
        }
        let edits = item
            .get("additionalTextEdits")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(extra_edit).collect())
            .unwrap_or_default();
        if !ranged {
            if let Some(range) = edit.and_then(|e| e.get("range")) {
                start = character_column(caret_line, range.get("start").and_then(|s| s.get("character")).and_then(Value::as_u64).unwrap_or(0) as usize);
                end = character_column(caret_line, range.get("end").and_then(|s| s.get("character")).and_then(Value::as_u64).unwrap_or(0) as usize);
                ranged = true;
            }
        }
        let kind = item
            .get("kind")
            .and_then(Value::as_i64)
            .map(kind_of)
            .unwrap_or(Kind::Word);
        let note = item
            .get("detail")
            .and_then(Value::as_str)
            .map(shorten)
            .filter(|line| !line.is_empty() && *line != name);
        let item = Item {
            name: name.clone(),
            text,
            kind,
            note,
            call,
            edits,
        };
        // Two servers will hand back the same name twice when it comes from two
        // declarations, and the panel has no way to tell the reader why.
        if !seen.contains(&item.name) {
            seen.insert(item.name.clone());
            found.push(item);
            if found.len() == POOL {
                break;
            }
        }
    }
    // Inside an `#include` the answer carries the delimiter that closes the
    // directive (`stdios.h"`), because accepting has to close it. The row is a
    // header name, not a directive, so the closer stays out of it.
    if let Some(closer) = closing_delimiter(caret_line, start) {
        for item in &mut found {
            if item.name.ends_with(closer) {
                item.name.pop();
            }
        }
    }
    if !ranged {
        // No textEdit means the server expects the client's own word boundary,
        // which is the run of name letters behind the caret and nothing more.
        start = column - name_before(caret_line, column).chars().count();
        end = column;
    }
    Answer {
        items: found,
        start,
        end,
        incomplete: raw
            .get("isIncomplete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// The row's own text: the label the server chose to be read, with the padding
/// whitespace clangd writes into every one of them taken off. A label that still
/// carries snippet markup is not a name, and there the row is named by what
/// stands before the call opens.
fn listed(label: &str, text: &str) -> String {
    let shown = label.trim();
    if shown.is_empty() || shown.contains('$') {
        head(text, label)
    } else {
        shown.to_string()
    }
}

/// A server's snippet, unrolled: the text it would type, and the placeholders
/// inside it as character offsets, numbered stops first and the final caret
/// (`$0`) last. `printf(${1:const char *fmt})` becomes the text
/// `printf(const char *fmt)` with the parameter selected, so typing replaces
/// the type text instead of landing after it.
fn snippet(text: &str) -> (String, Vec<(usize, usize)>) {
    let (out, mut stops) = unrolled(text);
    // A snippet with holes and no `$0` ends where its text ends, so the last
    // Tab lands after the closing bracket rather than nowhere.
    let end = out.chars().count();
    if !stops.is_empty() && !stops.iter().any(|(number, _, _)| *number == 0) {
        stops.push((0, end, end));
    }
    stops.sort_by_key(|(number, _, _)| if *number == 0 { u32::MAX } else { *number });
    (out, stops.into_iter().map(|(_, start, end)| (start, end)).collect())
}

/// The same, without deciding where the walk ends: nested holes are unrolled
/// with their numbers intact so the outer snippet can carry them.
fn unrolled(text: &str) -> (String, Vec<(u32, usize, usize)>) {
    let mut out = String::with_capacity(text.len());
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    let mut at = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                    at += 1;
                }
            }
            '$' => match chars.peek().copied() {
                Some('{') => {
                    chars.next();
                    let mut digits = String::new();
                    while let Some(digit) = chars.peek().copied().filter(|d| d.is_ascii_digit()) {
                        digits.push(digit);
                        chars.next();
                    }
                    let Ok(number) = digits.parse::<u32>() else {
                        out.push_str("${");
                        at += 2;
                        continue;
                    };
                    match chars.next() {
                        Some(':') => {
                            // Everything up to the brace that closes this hole,
                            // braces inside the default text nested within it.
                            let mut depth = 1;
                            let mut body = String::new();
                            while let Some(inner) = chars.next() {
                                match inner {
                                    '\\' => {
                                        if let Some(escaped) = chars.next() {
                                            body.push('\\');
                                            body.push(escaped);
                                        }
                                    }
                                    '{' => {
                                        depth += 1;
                                        body.push(inner);
                                    }
                                    '}' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            break;
                                        }
                                        body.push(inner);
                                    }
                                    _ => body.push(inner),
                                }
                            }
                            let (default, nested) = unrolled(&body);
                            let start = at;
                            at += default.chars().count();
                            out.push_str(&default);
                            for (inner_number, inner_start, inner_end) in nested {
                                stops.push((inner_number, start + inner_start, start + inner_end));
                            }
                            stops.push((number, start, at));
                        }
                        Some('|') => {
                            let mut options = String::new();
                            while let Some(inner) = chars.next() {
                                if inner == '|' && chars.peek() == Some(&'}') {
                                    chars.next();
                                    break;
                                }
                                options.push(inner);
                            }
                            let first = options.split(',').next().unwrap_or("");
                            let start = at;
                            at += first.chars().count();
                            out.push_str(first);
                            stops.push((number, start, at));
                        }
                        Some('}') => stops.push((number, at, at)),
                        Some(other) => {
                            out.push(other);
                            at += 1;
                        }
                        None => {}
                    }
                }
                Some(digit) if digit.is_ascii_digit() => {
                    let mut value = 0u32;
                    while let Some(digit) = chars.peek().copied().filter(|d| d.is_ascii_digit()) {
                        chars.next();
                        value = value.saturating_mul(10).saturating_add(digit as u32 - '0' as u32);
                    }
                    stops.push((value, at, at));
                }
                _ => {
                    out.push('$');
                    at += 1;
                }
            },
            _ => {
                out.push(ch);
                at += 1;
            }
        }
    }
    (out, stops)
}

/// The name a snippet is listed under: everything before the call opens. The
/// row has to stay a name, because that is what the reader is typing.
fn head(text: &str, label: &str) -> String {
    let name: String = text
        .chars()
        .take_while(|ch| !ch.is_whitespace() && *ch != '(' && *ch != '<')
        .collect();
    if name.is_empty() {
        shorten(label)
    } else {
        name
    }
}

/// One `additionalTextEdits` entry, in the server's own coordinates. A server
/// that sends something that is not an edit is a server whose edit is skipped.
fn extra_edit(raw: &Value) -> Option<ExtraEdit> {
    let range = raw.get("range")?;
    let text = raw.get("newText").and_then(Value::as_str)?;
    let at = |side: &Value| {
        Some((
            side.get("line").and_then(Value::as_u64)? as usize,
            side.get("character").and_then(Value::as_u64)?,
        ))
    };
    Some(ExtraEdit {
        start: at(range.get("start")?)?,
        end: at(range.get("end")?)?,
        text: text.to_string(),
    })
}

/// The run of name letters behind the caret. Kite needs it to narrow an answer
/// that has already arrived while the next one is still coming; a server needs
/// it as the fallback range when it answers without one.
pub(crate) fn name_before(line: &str, column: usize) -> String {
    let before: Vec<char> = line.chars().take(column).collect();
    let taken = before
        .iter()
        .rev()
        .take_while(|ch| ch.is_alphanumeric() || **ch == '_')
        .count();
    before[before.len() - taken..].iter().collect()
}

/// kite counts characters, a server counts UTF-16 code units. Those agree on
/// everything but the astral plane — an emoji is one character and two units —
/// and the conversion is done by walking the line's own prefix rather than by
/// assuming either counting is the other's.
fn utf16_column(line: &str, column: usize) -> usize {
    line.chars()
        .take(column)
        .map(|ch| ch.len_utf16())
        .sum()
}

pub(crate) fn character_column(line: &str, units: usize) -> usize {
    let mut seen = 0;
    for (index, ch) in line.chars().enumerate() {
        if seen >= units {
            return index;
        }
        seen += ch.len_utf16();
    }
    line.chars().count()
}

/// The server's number for what a name is, into kite's. The numbering is the
/// protocol's own; a row that got it wrong labels a class as a variable, which
/// is a worse nothing than a badge that says only `word`.
fn kind_of(value: i64) -> Kind {
    match value {
        2 | 3 | 4 => Kind::Function,
        5 | 11 => Kind::Member,
        6 | 13 => Kind::Variable,
        7 | 8 => Kind::Struct,
        9 | 14 | 21 => Kind::Type,
        10 | 12 => Kind::Module,
        15 => Kind::Keyword,
        // 17 is Color in the spec, but clangd numbers the headers it offers
        // after `#include <` with it, and a header is a path whatever the server
        // calls it. No language kite completes on has colours.
        17 | 18 | 19 | 20 => Kind::Path,
        _ => Kind::Word,
    }
}

/// A signature is the useful half of a note and the long half of a line.
fn shorten(text: &str) -> String {
    let one_line = text.lines().next().unwrap_or("").trim();
    let chars: Vec<char> = one_line.chars().collect();
    if chars.len() <= 46 {
        return one_line.to_string();
    }
    chars[..43].iter().collect::<String>() + "…"
}

/// The ancestor that declares itself a project, or the folder the file is in.
fn project_root(path: &Path) -> PathBuf {
    let here = path.parent().map(Path::to_path_buf).unwrap_or_default();
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "compile_commands.json",
        "CMakeLists.txt",
        "Makefile",
    ];
    let mut cursor = here.as_path();
    loop {
        if MARKERS.iter().any(|marker| cursor.join(marker).exists()) {
            return cursor.to_path_buf();
        }
        let Some(parent) = cursor.parent() else {
            return if here.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                here
            };
        };
        cursor = parent;
    }
}

/// A `file:` URI. Percent-encoding is not decoration: this machine's home
/// folder is written in characters a URI may not carry raw.
fn uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut body = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | ':' | '.' | '_' | '-') {
            body.push(ch);
            continue;
        }
        let mut bytes = [0u8; 4];
        for byte in ch.encode_utf8(&mut bytes).as_bytes() {
            body.push_str(&format!("%{byte:02X}"));
        }
    }
    if body.starts_with('/') {
        format!("file://{body}")
    } else {
        format!("file:///{body}")
    }
}

/// Which page a server means by a `file:` URI, in the one spelling both sides
/// can arrive at. A language server answers for the file it resolved the URI to,
/// and on this machine that resolution is a different string from the one kite
/// was handed: a home folder written in characters a URI must escape also has an
/// 8.3 short name, and clangd reports against that one. Asking the filesystem
/// which spelling is which is the only way the two end up on the same row; a
/// page that was never on disk has nothing to ask, so it keeps its own.
fn page_key(path: &Path) -> String {
    let named = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut text = plain(&named);
    // Windows does not care how the letters of a path are written, and the two
    // ways of casing one folder must not become two entries in the table.
    if cfg!(windows) {
        text.make_ascii_lowercase();
    }
    while text.ends_with('/') {
        text.pop();
    }
    text
}

/// A path as a person reads it: the `\\?\` the kernel hands back is namespace
/// bookkeeping, and forward slashes are what a URI is written with anyway.
fn plain(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let bare = match raw.strip_prefix(r"\\?\UNC\") {
        Some(remote) => format!(r"\\{remote}"),
        None => raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string(),
    };
    bare.replace('\\', "/")
}

/// The inverse of [`uri`], and only nearly so: a server escapes whatever it
/// likes, so anything that is not a plain local file is refused rather than
/// guessed at. A report about somebody else's machine is not this page's.
fn path_from_uri(raw: &str) -> Option<PathBuf> {
    let body = decoded(raw.strip_prefix("file://")?)?;
    let at = body.as_bytes();
    if at.first() != Some(&b'/') {
        // A URI whose slashes are followed by a name is a share, and no page
        // kite has ever opened lives on one.
        return None;
    }
    // `file:///C:/x` carries the slash that the third of its slashes stands for.
    let drive = at.get(1).is_some_and(|letter| letter.is_ascii_alphabetic())
        && at.get(2) == Some(&b':');
    Some(PathBuf::from(if drive { &body[1..] } else { &body }))
}

/// Percent-decoding, byte by byte, because the escaped bytes are UTF-8 and not
/// characters. A `%` that does not start a hex pair stays the literal it was.
fn decoded(text: &str) -> Option<String> {
    let raw = text.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' && raw.len() >= at + 3 {
            let pair = std::str::from_utf8(&raw[at + 1..at + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = pair {
                bytes.push(byte);
                at += 3;
                continue;
            }
        }
        bytes.push(raw[at]);
        at += 1;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_survives_its_own_framing() {
        // The frame is bytes on a pipe, so the length has to count bytes and not
        // characters — which is the whole reason a Chinese comment matters here.
        let message = json!({"method": "textDocument/didOpen", "params": {"text": "补全"}});
        let body = serde_json::to_vec(&message).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n", body.len());
        assert_eq!(framed.len() + body.len(), framed.as_bytes().len() + body.len());
        assert!(body.len() > message.to_string().chars().count());
    }

    #[test]
    fn an_outgoing_message_carries_the_version() {
        // A notification written by hand is the one that forgets the field, and
        // a server that fails the version check says nothing at all in reply.
        let mut exit = json!({"method": "exit"});
        stamp(&mut exit);
        assert_eq!(exit.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        let mut versioned = json!({"jsonrpc": "2.0", "method": "exit"});
        stamp(&mut versioned);
        assert_eq!(versioned, json!({"method": "exit", "jsonrpc": "2.0"}));
    }

    /// kite counts characters and a server counts UTF-16 code units. Most of the
    /// time those are the same number — a Chinese character is one of each, which
    /// is why the comments in these files shift nothing — but an emoji is one
    /// character and two units, and a column that is off by one there puts the
    /// answer a letter down the wrong end of the row.
    #[test]
    fn columns_are_counted_the_way_each_side_counts_them() {
        let han = "// 补全 printf";
        assert_eq!(utf16_column(han, 5), 5, "one character, one code unit");
        assert_eq!(character_column(han, 5), 5);
        let emoji = "let 🐧icon = 1;";
        assert_eq!(utf16_column(emoji, 5), 6, "the penguin is two units wide");
        assert_eq!(character_column(emoji, 6), 5);
        for line in [han, emoji] {
            for column in 0..=line.chars().count() {
                assert_eq!(character_column(line, utf16_column(line, column)), column);
            }
        }
    }

    /// What a server reports, and what is worth a line on the page. The protocol
    /// has four severities; the page has two colours, and the two below those
    /// are the ones that keep a reader from seeing the trouble that matters.
    #[test]
    fn a_report_keeps_the_trouble_and_drops_the_rest() {
        let side = |line, units| json!({"line": line, "character": units});
        let error = raw_diagnostic(&json!({
            "range": {"start": side(3, 2), "end": side(3, 7)},
            "severity": 1, "source": "clang", "code": "expected_semi_declaration",
            "message": "Expected ';' at end of declaration"}))
            .expect("an error is worth drawing");
        assert_eq!(error.start, (3, 2));
        assert_eq!(error.end, (3, 7));
        assert!(error.error);
        assert_eq!(error.note, "clang expected_semi_declaration");

        let warning = raw_diagnostic(&json!({
            "range": {"start": side(5, 2), "end": side(5, 16)},
            "severity": 2, "code": "-Wimplicit", "message": "Call to undeclared function"}))
            .map(|note| (note.error, note.note));
        assert_eq!(
            warning,
            Some((false, "-Wimplicit".to_string())),
            "a doubt is drawn, and drawn as a doubt"
        );

        for quiet in [
            json!({"range": {"start": side(0, 0), "end": side(0, 1)}, "severity": 3,
                   "message": "use auto"}),
            json!({"range": {"start": side(0, 0), "end": side(0, 1)}, "severity": 4,
                   "message": "prefer"}),
            json!({"range": {"start": side(0, 0), "end": side(0, 1)}, "message": ""}),
            json!({"severity": 1, "message": "no range at all"}),
            // Measured 2026-10-31 against clangd 23.1.0: this is exactly what it
            // sends for an include the page names nothing from. The protocol tag
            // is what identifies the class of it, so the rule covers math.h,
            // string.h and whatever a server invents next without a name list.
            json!({"range": {"start": side(0, 0), "end": side(0, 18)}, "severity": 2,
                   "source": "clangd", "code": "unused-includes", "tags": [1],
                   "message": "Included header stdio.h is not used directly (fix available)"}),
        ] {
            assert!(raw_diagnostic(&quiet).is_none(), "{quiet} is not a trouble");
        }
        // Deprecated is a different claim: the program still works, but it is
        // built on something that is going away. That one is drawn.
        assert!(
            raw_diagnostic(&json!({
                "range": {"start": side(1, 0), "end": side(1, 5)}, "severity": 2,
                "tags": [2], "message": "'foo' is deprecated"}))
                .is_some(),
            "a doubt about the future is still a doubt about the program"
        );
    }

    /// Reports are filed by page, and the page a server names is not always
    /// spelled the way kite was handed it. The URI has to survive the trip out
    /// and back, and both spellings of one file have to land on one row.
    #[test]
    fn a_report_lands_on_the_page_it_names() {
        let home = Path::new(r"C:\Users\清枫不识客\AppData\Local\Temp\kite\broken.c");
        let sent = uri(home);
        assert!(sent.contains("%E6%B8"), "the folder is escaped: {sent}");
        assert_eq!(path_from_uri(&sent).as_deref(), Some(home));
        for elsewhere in ["untitled:Untitled-1", "file://server/share/a.c"] {
            assert!(path_from_uri(elsewhere).is_none(), "{elsewhere} is not a page");
        }
        // The disk is what makes two spellings of one file the same page: the
        // short name a server resolves to and the long one kite holds have to
        // find each other in one table.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.c");
        std::fs::write(&file, "int a\n").unwrap();
        let echoed = uri(&file);
        assert_eq!(
            page_key(&path_from_uri(&echoed).unwrap()),
            page_key(&file),
            "and a resolved path still means this page"
        );
        assert_eq!(
            page_key(&std::fs::canonicalize(&file).unwrap()),
            page_key(&file),
            "with or without the kernel's prefix on it"
        );
    }

    #[test]
    fn a_snippet_unrolls_into_text_and_the_places_tab_walks_through() {
        assert_eq!(
            snippet("printf(${1:const char *fmt})"),
            ("printf(const char *fmt)".to_string(), vec![(7, 22), (23, 23)])
        );
        assert_eq!(
            snippet("foo($1, $2)"),
            ("foo(, )".to_string(), vec![(4, 4), (6, 6), (7, 7)])
        );
        assert_eq!(
            snippet("vec.${1:push}${2:()}"),
            ("vec.push()".to_string(), vec![(4, 8), (8, 10), (10, 10)])
        );
        assert_eq!(
            snippet("if (${1|true,false|})"),
            ("if (true)".to_string(), vec![(4, 8), (9, 9)])
        );
        assert_eq!(
            snippet("a ${1:outer ${2:inner}} b"),
            ("a outer inner b".to_string(), vec![(2, 13), (8, 13), (15, 15)]),
            "an inner hole keeps its own place and the outer one still walks first"
        );
        assert_eq!(
            snippet("printf(fmt)"),
            ("printf(fmt)".to_string(), Vec::new()),
            "text with no holes is text, and Tab has nothing to walk"
        );
    }

    #[test]
    fn a_snippet_is_listed_under_its_name() {
        assert_eq!(head("printf(const char *fmt)", " printf(...)"), "printf");
        assert_eq!(head("println!(${1:x})", " println!"), "println!");
        assert_eq!(head("(int)value", " (int)value"), "(int)value");
    }

    #[test]
    fn an_include_edit_keeps_the_servers_coordinates() {
        let raw = json!({"range": {"start": {"line": 0, "character": 0},
                                   "end": {"line": 0, "character": 0}},
                         "newText": "#include <string.h>\n"});
        assert_eq!(
            extra_edit(&raw),
            Some(ExtraEdit {
                start: (0, 0),
                end: (0, 0),
                text: "#include <string.h>\n".to_string()
            })
        );
        assert_eq!(extra_edit(&json!({"newText": "x"})), None);
    }

    #[test]
    fn a_padded_label_still_gives_the_plain_name() {
        // clangd labels every item with a leading space and puts the clean name
        // in insertText. A ghost taken from the label would start with a space
        // the row does not have, and the prefix filter would reject every item
        // the server sent — an empty completion list and no clue why.
        let raw = json!({"items": [
            {"label": " printf", "insertText": "printf", "kind": 3, "detail": "int printf(const char * restrict, ...)"},
            {"label": " puts", "insertText": "puts", "kind": 3}]});
        let got = answer(&raw, "  prin", 6);
        let names: Vec<&str> = got.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(names, ["printf", "puts"]);
        assert_eq!(got.start, 2);
        assert_eq!(got.items[0].note.as_deref(), Some("int printf(const char * restrict, ...)"));
    }

    #[test]
    fn a_completion_list_and_a_bare_array_both_answer() {
        let list = json!({"isIncomplete": false, "items": [
            {"label": "printf", "kind": 3, "detail": "int printf(const char *)"}]});
        let bare = json!([{"label": "printf", "kind": 3}]);
        for raw in [list, bare] {
            let found = answer(&raw, "print", 5);
            assert_eq!(found.items.len(), 1, "{raw:?}");
            assert_eq!(found.items[0].text, "printf");
            assert_eq!(found.items[0].kind, Kind::Function);
            assert_eq!(found.start, 0, "no textEdit, so the word behind the caret");
        }
    }

    #[test]
    fn a_text_edit_range_decides_where_the_pick_lands() {
        let raw = json!({"items": [{"label": "printf", "insertText": "printf",
            "textEdit": {"newText": "printf", "range": {
                "start": {"line": 0, "character": 1},
                "end": {"line": 0, "character": 5}}}}]});
        let found = answer(&raw, "xprint", 6);
        assert_eq!((found.start, found.end), (1, 5));
    }

    #[test]
    fn the_same_name_from_two_declarations_is_one_row() {
        let raw = json!({"items": [
            {"label": "init", "kind": 3},
            {"label": "init", "kind": 3, "detail": "other.c"},
            {"label": "index", "kind": 3}]});
        let found = answer(&raw, "ini", 3);
        let names: Vec<String> = found.items.into_iter().map(|item| item.text).collect();
        assert_eq!(names, vec!["init", "index"]);
    }

    #[test]
    fn a_language_without_a_server_is_left_alone() {
        // No fallback on purpose: a list of the words already in the file is what
        // kite used to offer, and it was worse than an empty row.
        assert!(launch("markdown").is_none());
        assert!(launch("bash").is_none());
        assert!(launch("javascript").is_none());
    }

    /// clangd names `. < > : " / *` and not `(`. That list is the difference
    /// between a dot meaning "ask me about this object" and an opening bracket
    /// meaning "here is every name visible from here", and which one it is is
    /// the server's to say rather than the client's to guess from the shape of
    /// the character. A quote and an angle bracket are triggers like any other:
    /// typing `#include "` is exactly when the headers are wanted.
    #[test]
    fn the_server_names_its_own_trigger_characters() {
        let reply = json!({"id": 0, "result": {"capabilities": {"completionProvider": {
            "triggerCharacters": [".", "<", ">", ":", "\"", "/", "*"]}}}});
        let triggers = named_triggers(&reply);
        assert_eq!(
            triggers.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![".", "<", ">", ":", "\"", "/", "*"]
        );
        assert!(declared(&triggers, Some('.')));
        assert!(declared(&triggers, Some(':')));
        assert!(declared(&triggers, Some('"')), "the quote asks for headers");
        assert!(declared(&triggers, Some('<')), "and so does the bracket");
        assert!(!declared(&triggers, Some('(')), "a call's bracket scopes nothing");
        assert!(!declared(&triggers, Some('-')));
        assert!(!declared(&triggers, Some(' ')));
        assert!(!declared(&triggers, None), "the row has not been started");
        assert!(named_triggers(&json!({"result": {}})).is_empty());
    }

    /// The include answer is a directive, the row is a header name.
    #[test]
    fn a_header_row_drops_the_delimiter_the_answer_carries() {
        let raw = json!({"items": [
            {"label": " stdio.h\"", "kind": 17,
             "textEdit": {"newText": "stdio.h\"",
                "range": {"start": {"line": 0, "character": 10},
                          "end": {"line": 0, "character": 11}}}}
        ]});
        let quoted = answer(&raw, "#include \"", 10);
        assert_eq!(quoted.start, 10);
        assert_eq!(quoted.items[0].name, "stdio.h", "the row is the name");
        assert_eq!(
            quoted.items[0].text, "stdio.h\"",
            "accepting still closes the quote"
        );
        let angled = json!({"items": [
            {"label": " stdio.h>", "kind": 17,
             "textEdit": {"newText": "stdio.h>",
                "range": {"start": {"line": 0, "character": 10},
                          "end": {"line": 0, "character": 10}}}}
        ]});
        let bracketed = answer(&angled, "#include <", 10);
        assert_eq!(bracketed.items[0].name, "stdio.h");
    }

    #[test]
    fn a_path_with_characters_a_uri_cannot_carry_is_encoded() {
        let text = uri(Path::new(r"C:\Users\清枫\main.c"));
        assert!(text.starts_with("file:///C:/Users/"), "{text}");
        assert!(!text.chars().any(|ch| ch > '\u{7f}'), "{text}");
        assert!(text.contains("%E6%B8%85"), "{text}");
    }

    #[test]
    fn the_project_is_the_ancestor_that_declares_itself_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\n").unwrap();
        let deep = dir.path().join("src").join("main.rs");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        std::fs::write(&deep, b"fn main(){}\n").unwrap();
        assert_eq!(project_root(&deep), dir.path());
    }

    /// Two clangds unpacked into the same folder is a real state on a real
    /// machine, and the one offered has to be the newer of the two.
    #[test]
    fn a_newer_version_of_a_server_outranks_an_older_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut dirs: Vec<PathBuf> = ["clangd_9.0.0", "clangd_23.1.0", "18.1.3"]
            .iter()
            .map(|name| dir.path().join(name).join("bin"))
            .collect();
        for bin in dirs.clone() {
            std::fs::create_dir_all(bin).unwrap();
        }
        let version = |path: &Path| -> Vec<u64> {
            version_parts(path.parent().unwrap().file_name().unwrap())
        };
        dirs.sort_by(|a, b| version(b).cmp(&version(a)));
        let names: Vec<String> = dirs
            .iter()
            .map(|bin| bin.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["clangd_23.1.0", "18.1.3", "clangd_9.0.0"]);
    }

    #[test]
    fn an_npm_shim_names_the_script_it_runs() {
        // An npm bin is a batch file that knows where the server went, and it is
        // the only thing that does. Windows will not start it, so the path is
        // read out of it and node is pointed at that instead.
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("pyright-langserver.cmd");
        std::fs::write(
            &shim,
            "@ECHO off\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \
             \"%_prog%\"  \"%dp0%\\node_modules\\pyright\\langserver.index.js\" %*\n",
        )
        .unwrap();
        assert_eq!(
            npm_script(&shim).unwrap(),
            dir.path().join("node_modules/pyright/langserver.index.js")
        );
    }

    /// The rest of these tests hand in a reply as though a server had sent one.
    /// This one runs the actual thing: spawns the server, frames the handshake,
    /// opens a file, asks, and waits on the pipe. It is `#[ignore]`d because it
    /// needs a server installed on the machine and takes seconds rather than
    /// milliseconds — run it on purpose with `cargo test -p kite-app -- --ignored`.
    fn real_names(
        language: &'static str,
        file: &str,
        text: &str,
        line: usize,
        column: usize,
        wanted: &str,
    ) -> Vec<String> {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file);
        std::fs::write(&path, text).unwrap();
        let caret_line = text.lines().nth(line).unwrap_or("").to_string();

        let servers = Servers::new();
        let started = Instant::now();
        let mut asked = None;
        let mut last: Vec<String> = Vec::new();
        loop {
            if asked.is_none() {
                asked = servers.ask(
                    &Page {
                        path: &path,
                        language,
                        text,
                        revision: 1,
                    },
                    &Asking {
                        caret_line: &caret_line,
                        line,
                        column,
                        trigger: None,
                    },
                );
            }
            // The first answer after a file opens is the thin one: the server has
            // the text and no index yet, so it hands back the words in the file
            // instead of what the language knows. A person keeps typing while the
            // index is built and every keystroke asks again, so this loop does
            // the same rather than trusting the first reply.
            if let Some(id) = asked {
                if let Some(answer) = servers.take(id, &caret_line, column) {
                    last = answer.items.into_iter().map(|item| item.text).collect();
                    let hit = last.iter().any(|name| name.trim_end_matches('>').ends_with(wanted));
                    if hit {
                        println!(
                            "the {language} server answered {} names in {:?}: {:?}",
                            last.len(),
                            started.elapsed(),
                            &last[..last.len().min(5)]
                        );
                        servers.quit();
                        return last;
                    }
                    // The thin answer: ask again rather than leave the file cold.
                    asked = None;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no answer matching {wanted} in 60 s (last answer: {last:?})"
            );
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    #[test]
    #[ignore]
    fn a_real_c_server_answers_over_a_real_pipe() {
        let names = real_names("c", "main.c", "#include <\nint main(void) { return 0; }\n", 0, 10, ".h");
        assert!(
            names.iter().any(|name| name.trim_end_matches('>').ends_with(".h")),
            "no header among {names:?}"
        );
    }

    /// The list replaces the word it was asked about, so the span the server
    /// names has to cover that word. clangd names all of a one-letter word, and
    /// it reaches past the caret to take a closing `>` along with the typed
    /// header — the accept path trusts both of these numbers.
    #[test]
    #[ignore]
    fn a_real_c_server_names_the_span_the_list_will_replace() {
        use std::time::{Duration, Instant};

        fn span(file: &str, text: &str, line: usize, column: usize) -> (usize, usize) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(file);
            std::fs::write(&path, text).unwrap();
            let caret_line = text.lines().nth(line).unwrap_or("").to_string();
            let servers = Servers::new();
            let started = Instant::now();
            let mut asked = None;
            loop {
                if asked.is_none() {
                    asked = servers.ask(
                        &Page {
                            path: &path,
                            language: "c",
                            text,
                            revision: 1,
                        },
                        &Asking {
                            caret_line: &caret_line,
                            line,
                            column,
                            trigger: None,
                        },
                    );
                }
                if let Some(id) = asked {
                    if let Some(answer) = servers.take(id, &caret_line, column) {
                        if !answer.items.is_empty() {
                            servers.quit();
                            return (answer.start, answer.end);
                        }
                        // The thin answer: ask again rather than trust a list
                        // that has no index behind it yet.
                        asked = None;
                    }
                }
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "no answer in 30 s for {file}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        assert_eq!(
            span("short.c", "int main(void) {\n  c\n}\n", 1, 3),
            (2, 3),
            "one letter is still a word the list will replace"
        );
        assert_eq!(span("long.c", "int main(void) {\n  comp\n}\n", 1, 6), (2, 6));
        assert_eq!(
            span("hdr.c", "#include <std>\n", 0, 13),
            (10, 14),
            "the `>` after the caret goes with the header it closes"
        );
    }

    #[test]
    #[ignore]
    fn a_real_python_server_answers_over_a_real_pipe() {
        let text = "class Ledger:\n    def __init__(self):\n        self.total = 0\n\nl = Ledger()\nl.\n";
        let names = real_names("python", "main.py", text, 5, 2, "total");
        assert!(names.iter().any(|name| name == "total"), "no field among {names:?}");
    }

    /// The report path end to end: a real clangd handed a file with a missing
    /// semicolon said so without being asked, and said so again when the file
    /// was mended. Completion answers a question; this is the server
    /// volunteering, which is a different wire and a different thread — and it
    /// has to follow the text, because a line under words that are no longer
    /// there is how a reader learns to ignore the red.
    #[test]
    #[ignore]
    fn a_real_c_server_keeps_its_report_on_the_page_as_it_stands() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.c");
        let broken = "#include <stdio.h>\nint main(void) {\n  int a\n  return 0;\n}\n";
        let mended =
            "#include <stdio.h>\nint main(void) {\n  int a;\n  printf(\"%d\", a);\n  return 0;\n}\n";
        let servers = Servers::new();
        std::fs::write(&path, broken).unwrap();
        let started = Instant::now();
        let found = loop {
            servers.sync(&Page {
                path: &path,
                language: "c",
                text: broken,
                revision: 1,
            });
            let told = servers.diagnostics(&path);
            if !told.is_empty() || started.elapsed() > Duration::from_secs(30) {
                break told;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(!found.is_empty(), "clangd said nothing about a missing ';'");
        let trouble = found.iter().find(|note| note.error).expect("an error");
        assert!(
            trouble.message.contains(';'),
            "the report is about the semicolon: {:?}",
            trouble.message
        );
        // Measured 2026-10-31 against clangd 23.1.0: the report is on the row
        // whose token proves the semicolon is missing — `return`, one row below
        // the declaration that forgot it — because that is where the parser ran
        // out of patience. A line on the declaration row would be kite's guess
        // at the trouble rather than the compiler's.
        assert_eq!(trouble.start.0, 3, "one row down from the caret");
        assert_eq!(trouble.end.0, 3, "and one row wide");
        assert!(trouble.note.contains("clang"), "and it says who said it");

        std::fs::write(&path, mended).unwrap();
        let started = Instant::now();
        let quiet = loop {
            servers.sync(&Page {
                path: &path,
                language: "c",
                text: mended,
                revision: 2,
            });
            let told = servers.diagnostics(&path);
            if !told.iter().any(|note| note.error)
                || started.elapsed() > Duration::from_secs(30)
            {
                break told;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        servers.quit();
        assert!(
            !quiet.iter().any(|note| note.error),
            "the ';' was written and the red stayed: {quiet:?}"
        );
    }

    /// The trigger list is the only part of the handshake kite keeps, and it is
    /// what keeps a ghost off the row after `(`. If it never arrives, nothing
    /// says so — punctuation completion just quietly stops — so a real server is
    /// asked what it declared.
    #[test]
    #[ignore]
    fn a_real_c_server_says_which_characters_ask_by_themselves() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trigger.c");
        let text = "int main(void) { return 0; }\n";
        std::fs::write(&path, text).unwrap();
        let servers = Servers::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            // Handing the page over is enough to run the handshake, which is the
            // only thing this test is waiting for.
            servers.sync(&Page {
                path: &path,
                language: "c",
                text,
                revision: 1,
            });
            if servers.declares(&path, "c", Some('<')) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            servers.declares(&path, "c", Some('<')),
            "the handshake's trigger list never reached the client"
        );
        assert!(
            !servers.declares(&path, "c", Some('(')),
            "a bracket is not a trigger clangd asked for"
        );
        servers.quit();
    }
}
