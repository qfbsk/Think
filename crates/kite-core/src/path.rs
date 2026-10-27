use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Case-insensitive canonical identity used to keep one tab per file.
///
/// This is intentionally path identity, not file identity: hard links and
/// externally renamed files remain distinct documents.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKey(String);

impl FileKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn file_key(path: &Path) -> io::Result<FileKey> {
    let canonical = fs::canonicalize(path)?;
    Ok(FileKey(identity(&canonical)))
}

/// Case- and separator-folded form of a path, so two spellings of the same file
/// compare equal. The file does not have to exist.
pub fn identity(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('/', "\\");
    while let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    text.to_lowercase()
}

pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
}

/// The same location as a plain Win32 path. `fs::canonicalize` hands back a
/// verbatim `\\?\` path, and a child process started in one cannot write to
/// its own directory: cmd calls it an unsupported UNC path and `ld` fails to
/// create its output. Anything stored for later, or handed to a child, goes
/// through here so the shape is right once and everywhere.
pub fn plain_path(path: &Path) -> PathBuf {
    let text = display_path(path);
    PathBuf::from(text)
}

/// Absolute save target that preserves the path the tab was opened with.
/// It does not resolve symlinks and does not require the file to exist.
pub fn absolute_bound_path(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
}

/// Where the shell and the sidebar start: the X drive, or the drive the
/// process was launched from when X: is not mounted.
pub fn start_dir() -> PathBuf {
    let x = PathBuf::from("X:\\");
    if x.is_dir() {
        return x;
    }
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("C:\\"));
    current.ancestors().last().unwrap_or(&current).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_does_not_require_existence() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("a").join("b.txt");
        let absolute = absolute_bound_path(&missing).unwrap();
        assert!(absolute.is_absolute());
        assert_eq!(absolute.file_name().unwrap(), "b.txt");
    }

    #[cfg(windows)]
    #[test]
    fn key_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("MixedCase.txt");
        fs::write(&file, b"x").unwrap();
        let upper = dir.path().join("MIXEDCASE.TXT");
        assert_eq!(file_key(&file).unwrap(), file_key(&upper).unwrap());
    }

    /// A child process has to be able to write in the directory it starts in,
    /// so the path handed to it is never the verbatim form canonicalize gives.
    #[cfg(windows)]
    #[test]
    fn a_canonicalized_path_becomes_one_a_child_can_work_in() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "canonicalize is verbatim: {canonical:?}"
        );
        let plain = plain_path(&canonical);
        assert!(!plain.to_string_lossy().starts_with(r"\\?\"));
        assert!(plain.is_dir(), "still the same directory: {plain:?}");
        // A drive root keeps its separator, so it stays a directory path.
        assert_eq!(plain_path(Path::new(r"\\?\C:\")), PathBuf::from(r"C:\"));
        assert!(plain_path(Path::new(r"\\?\C:\")).is_dir());
        // A UNC share loses only the prefix.
        assert_eq!(
            plain_path(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
    }
}
