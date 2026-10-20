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
    Ok(FileKey(normalize_identity(&canonical)))
}

fn normalize_identity(path: &Path) -> String {
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

/// Absolute save target that preserves the path the tab was opened with.
/// It does not resolve symlinks and does not require the file to exist.
pub fn absolute_bound_path(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
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
}
