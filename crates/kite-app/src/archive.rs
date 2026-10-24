//! Archive listings borrowed from the bsdtar that ships with Windows.
//!
//! The sidebar only ever wants the paths inside an archive, and `tar.exe -tf`
//! prints exactly those for zip, tar, 7z, rar and cab. Naming them is worth a
//! background process: it saves carrying a decoder for every format, including
//! one whose algorithm we may not even re-implement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

use kite_core::identity;

use crate::shell::NO_WINDOW;

const TAR: &str = r"C:\Windows\System32\tar.exe";

/// Extensions whose whole contents bsdtar can name. A bare `.gz` is not among
/// them: it holds one file, not a tree, and `tar.exe` refuses it.
const SUFFIXES: &[&str] = &[
    ".zip", ".tar", ".tgz", ".tbz2", ".txz", ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".7z",
    ".rar", ".cab",
];

/// Would bsdtar list this file as a tree of entries?
pub fn is_archive(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_lowercase();
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

pub struct Listing {
    stamp: Stamp,
    entries: Vec<String>,
}

impl Listing {
    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

/// One entry name, normalised to `/` separated segments. Anything that could
/// escape the archive — `..`, an absolute path — is dropped rather than shown.
fn clean(raw: &str) -> Option<String> {
    let raw = raw.trim_end_matches(['\r', '\n']);
    let absolute = raw.starts_with('/') || raw.contains(':');
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.strip_suffix('/').unwrap_or(raw).split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            _ => parts.push(part),
        }
    }
    (!absolute && !parts.is_empty()).then(|| parts.join("/"))
}

/// bsdtar writes entry names through the system ANSI code page, so a GBK
/// machine answers in GBK. Names outside it are already lost by the time the
/// bytes reach us; asking the same API the child used keeps the rest intact.
#[cfg(windows)]
fn decode(bytes: &[u8]) -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetACP() -> u32;
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            input: *const u8,
            input_len: i32,
            output: *mut u16,
            output_len: i32,
        ) -> i32;
    }
    let Ok(input_len) = i32::try_from(bytes.len()) else {
        return String::new();
    };
    let code_page = unsafe { GetACP() };
    let needed = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            input_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            input_len,
            wide.as_mut_ptr(),
            needed,
        )
    };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

fn query(path: &Path) -> Option<Listing> {
    use std::os::windows::process::CommandExt;

    let stamp = stamp_of(path)?;
    let run = Command::new(TAR)
        .arg("-tf")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Its warnings about names the code page cannot represent are ours to ignore.
        .stderr(Stdio::null())
        .creation_flags(NO_WINDOW)
        .output();
    let entries = match run {
        Ok(output) => decode(&output.stdout).lines().filter_map(clean).collect(),
        Err(_) => Vec::new(),
    };
    Some(Listing { stamp, entries })
}

pub struct Probe {
    cache: HashMap<String, Listing>,
    asked: HashMap<String, Stamp>,
    requests: Sender<PathBuf>,
    replies: Receiver<(String, Listing)>,
    in_flight: usize,
}

impl Probe {
    pub fn new() -> Self {
        let (requests, inbox) = mpsc::channel::<PathBuf>();
        let (out, replies) = mpsc::channel();
        thread::Builder::new()
            .name("archive-list".to_string())
            .spawn(move || {
                while let Some(path) = inbox.recv().ok() {
                    let key = identity(&path);
                    let Some(listing) = query(&path) else {
                        continue;
                    };
                    if out.send((key, listing)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn archive-list worker");
        Self {
            cache: HashMap::new(),
            asked: HashMap::new(),
            requests,
            replies,
            in_flight: 0,
        }
    }

    pub fn pending(&self) -> bool {
        self.in_flight > 0
    }

    /// Ask for `archive`'s entries unless a listing as fresh as the file is held
    /// already, or the same question is still on its way.
    pub fn want(&mut self, archive: &Path) {
        let Some(stamp) = stamp_of(archive) else {
            return;
        };
        let key = identity(archive);
        if self
            .cache
            .get(&key)
            .is_some_and(|listing| listing.stamp == stamp)
        {
            return;
        }
        if self.asked.get(&key) == Some(&stamp) {
            return;
        }
        self.asked.insert(key.clone(), stamp);
        if self.requests.send(archive.to_path_buf()).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Take whatever the worker has finished; `true` means a listing arrived.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for (key, listing) in self.replies.try_iter() {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.cache.insert(key, listing);
            changed = true;
        }
        changed
    }

    pub fn listing(&self, archive: &Path) -> Option<&Listing> {
        self.cache.get(&identity(archive))
    }

    /// Hold a listing nobody asked a child for, so a test can drive the tree
    /// without waiting on a process.
    #[cfg(test)]
    pub fn seed(&mut self, archive: &Path, entries: Vec<String>) {
        let stamp = stamp_of(archive).expect("the test wrote this file");
        self.cache
            .insert(identity(archive), Listing { stamp, entries });
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::process::CommandExt;

    use super::*;

    #[test]
    fn only_tree_shaped_archives_count() {
        assert!(is_archive(Path::new(r"X:\luo.zip")));
        assert!(is_archive(Path::new(r"X:\LUO.7Z")));
        assert!(is_archive(Path::new(r"X:\a.tar.gz")));
        assert!(!is_archive(Path::new(r"X:\a.gz")), "one file, not a tree");
        assert!(!is_archive(Path::new(r"X:\a.txt")));
    }

    #[test]
    fn entry_names_are_normalised() {
        assert_eq!(clean("./luo/001.c").as_deref(), Some("luo/001.c"));
        assert_eq!(clean("luo/").as_deref(), Some("luo"));
        assert_eq!(clean("a\\b\\c.txt").as_deref(), Some("a/b/c.txt"));
        assert_eq!(clean("./"), None);
        assert_eq!(clean(""), None);
        assert_eq!(clean(".hidden").as_deref(), Some(".hidden"));
    }

    #[test]
    fn names_that_escape_the_archive_are_dropped() {
        assert_eq!(clean("../outside.txt"), None);
        assert_eq!(clean("ok/../../outside.txt"), None);
        assert_eq!(clean("/etc/passwd"), None);
        assert_eq!(clean("C:/Windows/win.ini"), None);
    }

    #[test]
    fn bsdtar_lists_a_real_archive() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("src");
        std::fs::create_dir(&inner).unwrap();
        std::fs::create_dir(inner.join("deep")).unwrap();
        std::fs::write(inner.join("one.txt"), b"1\n").unwrap();
        std::fs::write(inner.join("deep").join("two.txt"), b"2\n").unwrap();
        let archive = dir.path().join("probe.zip");
        let status = Command::new(TAR)
            .arg("-a")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&inner)
            .arg(".")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .status()
            .unwrap();
        assert!(status.success(), "bsdtar should build the fixture");

        let listing = query(&archive).expect("a file we just wrote");
        let names = listing.entries();
        assert!(
            names.iter().any(|name| name == "one.txt"),
            "no leading ./ left: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "deep/two.txt"),
            "nested entries: {names:?}"
        );
    }
}
