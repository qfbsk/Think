use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::encoding::{EncodingKind, TextEncoding, Unencodable, UnsupportedEncoding};
use crate::path::{absolute_bound_path, display_path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub ch: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub start_line: usize,
    pub old_count: usize,
    pub new_count: usize,
}

impl Position {
    pub fn new(line: usize, ch: usize) -> Self {
        Self { line, ch }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub cursor: Position,
}

impl Selection {
    pub fn new(anchor: Position, cursor: Position) -> Self {
        Self { anchor, cursor }
    }

    pub fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            cursor: position,
        }
    }

    pub fn ordered(self) -> (Position, Position) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.cursor
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Original { start: usize, end: usize },
    Owned(Vec<u8>),
}

impl Piece {
    fn raw_len(&self) -> usize {
        match self {
            Piece::Original { start, end } => end - start,
            Piece::Owned(bytes) => bytes.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Eol {
    bytes: [u8; 4],
    len: u8,
}

impl Eol {
    const NONE: Self = Self {
        bytes: [0; 4],
        len: 0,
    };

    fn new(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= 4);
        let mut buffer = [0u8; 4];
        buffer[..bytes.len()].copy_from_slice(bytes);
        Self {
            bytes: buffer,
            len: bytes.len() as u8,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    pieces: Vec<Piece>,
    eol: Eol,
}

#[derive(Clone, Debug)]
struct EditRecord {
    start_line: usize,
    old_lines: Vec<Line>,
    new_lines: Vec<Line>,
    before: Selection,
    after: Selection,
}

#[derive(Debug)]
pub enum DocumentError {
    Io(io::Error),
    UnsupportedEncoding(UnsupportedEncoding),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocumentError::Io(error) => write!(f, "{error}"),
            DocumentError::UnsupportedEncoding(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DocumentError::Io(error) => Some(error),
            DocumentError::UnsupportedEncoding(error) => Some(error),
        }
    }
}

impl From<io::Error> for DocumentError {
    fn from(error: io::Error) -> Self {
        DocumentError::Io(error)
    }
}

#[derive(Debug)]
pub struct SaveError {
    pub path: PathBuf,
    pub source: io::Error,
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not save {}: {}",
            display_path(&self.path),
            self.source
        )
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

struct LineView {
    raw: Vec<u8>,
    spans: Vec<(usize, usize)>,
}

impl LineView {
    fn raw_offset(&self, ch: usize) -> usize {
        if ch == 0 {
            0
        } else if ch >= self.spans.len() {
            self.raw.len()
        } else {
            self.spans[ch].0
        }
    }
}

pub struct Document {
    path: PathBuf,
    original: Vec<u8>,
    encoding: TextEncoding,
    lines: Vec<Line>,
    default_eol: Eol,
    undo: Vec<EditRecord>,
    redo: Vec<EditRecord>,
    last_change: Option<Change>,
    revision: u64,
    saved_revision: u64,
}

impl Document {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DocumentError> {
        let bound = absolute_bound_path(path.as_ref())?;
        let bytes = fs::read(&bound)?;
        Self::from_bytes(bound, bytes)
    }

    pub fn from_bytes(path: impl Into<PathBuf>, bytes: Vec<u8>) -> Result<Self, DocumentError> {
        let detected = TextEncoding::detect(&bytes).map_err(DocumentError::UnsupportedEncoding)?;
        Self::from_parts(path.into(), bytes, detected.encoding)
    }

    pub fn from_bytes_with_encoding(
        path: impl Into<PathBuf>,
        bytes: Vec<u8>,
        encoding: TextEncoding,
    ) -> Result<Self, DocumentError> {
        Self::from_parts(path.into(), bytes, encoding)
    }

    fn from_parts(
        path: PathBuf,
        bytes: Vec<u8>,
        encoding: TextEncoding,
    ) -> Result<Self, DocumentError> {
        let bound = absolute_bound_path(&path)?;
        let content_start = encoding.bom().len();
        if content_start > bytes.len() || !bytes.starts_with(encoding.bom()) {
            return Err(DocumentError::UnsupportedEncoding(UnsupportedEncoding(
                "encoding does not match file BOM",
            )));
        }
        let (lines, default_eol) = split_lines(&bytes, content_start, encoding.kind());
        let default_eol = default_eol.unwrap_or_else(|| Eol::new(&encoding.default_eol()));
        Ok(Self {
            path: bound,
            original: bytes,
            encoding,
            lines,
            default_eol,
            undo: Vec::new(),
            redo: Vec::new(),
            last_change: None,
            revision: 0,
            saved_revision: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn encoding(&self) -> &TextEncoding {
        &self.encoding
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn is_modified(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn line_text(&self, index: usize) -> String {
        let raw = self.line_raw(index);
        self.encoding.decode_lossy(&raw)
    }

    pub fn line_char_len(&self, index: usize) -> usize {
        self.line_text(index).chars().count()
    }

    pub fn has_line_break(&self, index: usize) -> bool {
        self.lines
            .get(index)
            .is_some_and(|line| !line.eol.is_empty())
    }

    pub fn line_break_len(&self, index: usize) -> usize {
        self.lines
            .get(index)
            .map_or(0, |line| line.eol.len as usize)
    }

    pub fn end_position(&self) -> Position {
        let line = self.lines.len() - 1;
        Position::new(line, self.line_char_len(line))
    }

    pub fn select_all(&self) -> Selection {
        Selection::new(Position::default(), self.end_position())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn apply_edit(
        &mut self,
        selection: Selection,
        text: &str,
    ) -> Result<Selection, Unencodable> {
        let (start, end) = selection.ordered();
        let start = self.clamp(start);
        let end = self.clamp(end);
        if start == end && text.is_empty() {
            return Ok(Selection::caret(start));
        }

        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let inserted_parts: Vec<&str> = normalized.split('\n').collect();
        let mut encoded_parts = Vec::with_capacity(inserted_parts.len());
        for part in &inserted_parts {
            encoded_parts.push(self.encoding.encode_checked(part)?);
        }

        let start_view = self.line_view(start.line);
        let start_raw = start_view.raw_offset(start.ch);
        let (prefix, same_line_rest) = if start.line == end.line {
            let (prefix, rest) = split_pieces(&self.lines[start.line].pieces, start_raw);
            (prefix, Some(rest))
        } else {
            (
                split_pieces(&self.lines[start.line].pieces, start_raw).0,
                None,
            )
        };

        let (end_suffix, old_eol) = if start.line == end.line {
            let end_raw = start_view.raw_offset(end.ch);
            let rest = same_line_rest.expect("same-line suffix");
            (
                split_pieces(&rest, end_raw - start_raw).1,
                self.lines[start.line].eol,
            )
        } else {
            let end_view = self.line_view(end.line);
            let end_raw = end_view.raw_offset(end.ch);
            (
                split_pieces(&self.lines[end.line].pieces, end_raw).1,
                self.lines[end.line].eol,
            )
        };

        let mut new_lines = Vec::with_capacity(encoded_parts.len());
        for (index, encoded) in encoded_parts.iter().enumerate() {
            let is_first = index == 0;
            let is_last = index + 1 == encoded_parts.len();
            let mut pieces = Vec::new();
            if is_first {
                pieces.extend(prefix.iter().cloned());
            }
            if !encoded.is_empty() {
                push_owned(&mut pieces, encoded.clone());
            }
            if is_last {
                pieces.extend(end_suffix.iter().cloned());
            }
            let eol = if is_last { old_eol } else { self.default_eol };
            new_lines.push(Line { pieces, eol });
        }

        let last_part_chars = inserted_parts.last().map_or(0, |part| part.chars().count());
        let caret = if inserted_parts.len() == 1 {
            Position::new(start.line, start.ch + last_part_chars)
        } else {
            Position::new(start.line + inserted_parts.len() - 1, last_part_chars)
        };
        let before = Selection::new(start, end);
        let after = Selection::caret(caret);

        let old_lines: Vec<Line> = self.lines[start.line..=end.line].to_vec();
        let old_total = self.lines.len();
        let removed_count = end.line - start.line + 1;
        let added_count = new_lines.len();
        let record_lines = new_lines.clone();
        self.lines.splice(start.line..=end.line, new_lines);
        debug_assert_eq!(self.lines.len(), old_total - removed_count + added_count);

        self.undo.push(EditRecord {
            start_line: start.line,
            old_lines,
            new_lines: record_lines,
            before,
            after,
        });
        self.redo.clear();
        self.last_change = Some(Change {
            start_line: start.line,
            old_count: removed_count,
            new_count: added_count,
        });
        self.revision += 1;
        Ok(after)
    }

    pub fn undo(&mut self) -> Option<Selection> {
        let record = self.undo.pop()?;
        let end = record.start_line + record.new_lines.len();
        if end > self.lines.len() {
            self.undo.push(record);
            return None;
        }
        self.lines
            .splice(record.start_line..end, record.old_lines.iter().cloned());
        let selection = record.before;
        self.last_change = Some(Change {
            start_line: record.start_line,
            old_count: record.new_lines.len(),
            new_count: record.old_lines.len(),
        });
        self.redo.push(record);
        self.revision += 1;
        Some(selection)
    }

    pub fn redo(&mut self) -> Option<Selection> {
        let record = self.redo.pop()?;
        let end = record.start_line + record.old_lines.len();
        if end > self.lines.len() {
            self.redo.push(record);
            return None;
        }
        self.lines
            .splice(record.start_line..end, record.new_lines.iter().cloned());
        let selection = record.after;
        self.last_change = Some(Change {
            start_line: record.start_line,
            old_count: record.old_lines.len(),
            new_count: record.new_lines.len(),
        });
        self.undo.push(record);
        self.revision += 1;
        Some(selection)
    }

    pub fn take_change(&mut self) -> Option<Change> {
        self.last_change.take()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.original.len() + 64);
        out.extend_from_slice(self.encoding.bom());
        for line in &self.lines {
            for piece in &line.pieces {
                match piece {
                    Piece::Original { start, end } => {
                        out.extend_from_slice(&self.original[*start..*end]);
                    }
                    Piece::Owned(bytes) => out.extend_from_slice(bytes),
                }
            }
            out.extend_from_slice(line.eol.as_bytes());
        }
        out
    }

    pub fn save(&mut self) -> Result<(), SaveError> {
        let bytes = self.to_bytes();
        save_atomically(&self.path, &bytes).map_err(|source| SaveError {
            path: self.path.clone(),
            source,
        })?;
        self.saved_revision = self.revision;
        Ok(())
    }

    fn clamp(&self, position: Position) -> Position {
        let line = position.line.min(self.lines.len() - 1);
        let ch = position.ch.min(self.line_char_len(line));
        Position::new(line, ch)
    }

    fn line_raw(&self, index: usize) -> Vec<u8> {
        let line = &self.lines[index];
        let mut out = Vec::with_capacity(line.pieces.iter().map(Piece::raw_len).sum());
        for piece in &line.pieces {
            match piece {
                Piece::Original { start, end } => {
                    out.extend_from_slice(&self.original[*start..*end]);
                }
                Piece::Owned(bytes) => out.extend_from_slice(bytes),
            }
        }
        out
    }

    fn line_view(&self, index: usize) -> LineView {
        let raw = self.line_raw(index);
        let (text, spans) = if self.encoding.kind() == EncodingKind::Utf8 {
            decode_utf8_with_spans(&raw)
        } else {
            self.encoding.decode_with_spans(&raw)
        };
        debug_assert_eq!(text.chars().count(), spans.len());
        LineView { raw, spans }
    }
}

fn push_owned(pieces: &mut Vec<Piece>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    if let Some(Piece::Owned(previous)) = pieces.last_mut() {
        previous.extend_from_slice(&bytes);
    } else {
        pieces.push(Piece::Owned(bytes));
    }
}

fn split_pieces(pieces: &[Piece], at: usize) -> (Vec<Piece>, Vec<Piece>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut position = 0usize;
    for piece in pieces {
        let length = piece.raw_len();
        let end = position + length;
        if end <= at {
            left.push(piece.clone());
        } else if position >= at {
            right.push(piece.clone());
        } else {
            let local = at - position;
            match piece {
                Piece::Original { start, end } => {
                    if local > 0 {
                        left.push(Piece::Original {
                            start: *start,
                            end: start + local,
                        });
                    }
                    if local < length {
                        right.push(Piece::Original {
                            start: start + local,
                            end: *end,
                        });
                    }
                }
                Piece::Owned(bytes) => {
                    if local > 0 {
                        left.push(Piece::Owned(bytes[..local].to_vec()));
                    }
                    if local < length {
                        right.push(Piece::Owned(bytes[local..].to_vec()));
                    }
                }
            }
        }
        position = end;
    }
    (left, right)
}

fn split_lines(bytes: &[u8], content_start: usize, kind: EncodingKind) -> (Vec<Line>, Option<Eol>) {
    let mut lines = Vec::new();
    let mut first_eol = None;
    let mut position = content_start;
    loop {
        match find_eol(bytes, position, kind) {
            None => {
                lines.push(Line {
                    pieces: content_pieces(bytes, position, bytes.len()),
                    eol: Eol::NONE,
                });
                break;
            }
            Some((eol_start, eol_end, eol)) => {
                if first_eol.is_none() {
                    first_eol = Some(eol);
                }
                lines.push(Line {
                    pieces: content_pieces(bytes, position, eol_start),
                    eol,
                });
                position = eol_end;
                if position >= bytes.len() {
                    lines.push(Line {
                        pieces: Vec::new(),
                        eol: Eol::NONE,
                    });
                    break;
                }
            }
        }
    }
    (lines, first_eol)
}

fn content_pieces(bytes: &[u8], start: usize, end: usize) -> Vec<Piece> {
    if start < end && end <= bytes.len() {
        let mut pieces = Vec::with_capacity(1);
        pieces.push(Piece::Original { start, end });
        pieces
    } else {
        Vec::new()
    }
}

fn find_eol(bytes: &[u8], from: usize, kind: EncodingKind) -> Option<(usize, usize, Eol)> {
    match kind {
        EncodingKind::Utf16Le | EncodingKind::Utf16Be => {
            let mut position = from;
            while position + 1 < bytes.len() {
                let unit = if kind == EncodingKind::Utf16Le {
                    u16::from_le_bytes([bytes[position], bytes[position + 1]])
                } else {
                    u16::from_be_bytes([bytes[position], bytes[position + 1]])
                };
                if unit == 0x000A {
                    return Some((
                        position,
                        position + 2,
                        Eol::new(&bytes[position..position + 2]),
                    ));
                }
                if unit == 0x000D {
                    if position + 3 < bytes.len() {
                        let next = if kind == EncodingKind::Utf16Le {
                            u16::from_le_bytes([bytes[position + 2], bytes[position + 3]])
                        } else {
                            u16::from_be_bytes([bytes[position + 2], bytes[position + 3]])
                        };
                        if next == 0x000A {
                            return Some((
                                position,
                                position + 4,
                                Eol::new(&bytes[position..position + 4]),
                            ));
                        }
                    }
                    return Some((
                        position,
                        position + 2,
                        Eol::new(&bytes[position..position + 2]),
                    ));
                }
                position += 2;
            }
            None
        }
        _ => {
            for position in from..bytes.len() {
                if bytes[position] == b'\n' {
                    return Some((
                        position,
                        position + 1,
                        Eol::new(&bytes[position..position + 1]),
                    ));
                }
                if bytes[position] == b'\r' {
                    if position + 1 < bytes.len() && bytes[position + 1] == b'\n' {
                        return Some((
                            position,
                            position + 2,
                            Eol::new(&bytes[position..position + 2]),
                        ));
                    }
                    return Some((
                        position,
                        position + 1,
                        Eol::new(&bytes[position..position + 1]),
                    ));
                }
            }
            None
        }
    }
}

fn decode_utf8_with_spans(raw: &[u8]) -> (String, Vec<(usize, usize)>) {
    let mut text = String::with_capacity(raw.len());
    let mut spans = Vec::with_capacity(raw.len());
    let mut position = 0usize;
    while position < raw.len() {
        match std::str::from_utf8(&raw[position..]) {
            Ok(rest) => {
                for (offset, ch) in rest.char_indices() {
                    text.push(ch);
                    let start = position + offset;
                    spans.push((start, start + ch.len_utf8()));
                }
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let chunk = std::str::from_utf8(&raw[position..position + valid])
                        .expect("valid prefix");
                    for (offset, ch) in chunk.char_indices() {
                        text.push(ch);
                        let start = position + offset;
                        spans.push((start, start + ch.len_utf8()));
                    }
                    position += valid;
                }
                let invalid_len = error.error_len().unwrap_or(raw.len() - position);
                text.push('\u{FFFD}');
                spans.push((position, position + invalid_len));
                position += invalid_len;
            }
        }
    }
    debug_assert_eq!(text, String::from_utf8_lossy(raw));
    (text, spans)
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn save_atomically(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty());
    let directory = parent.unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map_or_else(|| "kite".into(), |name| name.to_string_lossy().into_owned());

    let mut temp_path = None;
    for _ in 0..128 {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = directory.join(format!(
            ".{file_name}.kite-save-{}-{counter}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                let result = file.write_all(bytes).and_then(|_| file.flush());
                drop(file);
                if let Err(error) = result {
                    let _ = fs::remove_file(&candidate);
                    return Err(error);
                }
                temp_path = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let temp_path = temp_path.ok_or_else(|| {
        io::Error::new(io::ErrorKind::AlreadyExists, "could not allocate save file")
    })?;
    if let Err(error) = fs::rename(&temp_path, target) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::GBK;

    fn docs_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn open_bytes(bytes: Vec<u8>) -> (tempfile::TempDir, PathBuf, Document) {
        let dir = docs_dir();
        let path = dir.path().join("sample.txt");
        fs::write(&path, &bytes).unwrap();
        let document = Document::open(&path).unwrap();
        (dir, path, document)
    }

    #[test]
    fn clean_roundtrip_preserves_mixed_eols_and_missing_final_newline() {
        let bytes = b"one\r\ntwo\nthree\rfour".to_vec();
        let (_dir, _path, mut document) = open_bytes(bytes.clone());
        assert_eq!(document.to_bytes(), bytes);
        assert_eq!(document.line_count(), 4);
        document.save().unwrap();
        assert_eq!(document.to_bytes(), bytes);
    }

    #[test]
    fn clean_roundtrip_preserves_utf8_bom() {
        let bytes = b"\xEF\xBB\xBFalpha\r\nbeta\n".to_vec();
        let (_dir, _path, document) = open_bytes(bytes.clone());
        assert!(document.encoding().has_bom());
        assert_eq!(document.to_bytes(), bytes);
    }

    #[test]
    fn editing_one_line_leaves_other_bytes_untouched() {
        let original = b"first\r\nsecond\nthird\r\nfourth";
        let (_dir, _path, mut document) = open_bytes(original.to_vec());
        let caret = Position::new(1, 3);
        document.apply_edit(Selection::caret(caret), "XYZ").unwrap();
        let saved = document.to_bytes();
        assert!(saved.starts_with(b"first\r\nsec"));
        assert!(saved.ends_with(b"ond\nthird\r\nfourth"));
        assert_eq!(document.line_text(1), "secXYZond");
        assert_eq!(document.line_text(2), "third");
    }

    #[test]
    fn ctrl_a_replacement_removes_every_original_byte() {
        let original = b"alpha\r\nbeta\n\xED\xA0\x80invalid\nomega".to_vec();
        let mut document = Document::from_bytes_with_encoding(
            std::env::temp_dir().join("ctrl-a.txt"),
            original.clone(),
            TextEncoding::utf8(),
        )
        .unwrap();
        let selection = document.select_all();
        document.apply_edit(selection, "replacement\nline").unwrap();
        assert_eq!(document.to_bytes(), b"replacement\r\nline");
        document.undo().unwrap();
        assert_eq!(document.to_bytes(), original);
    }

    #[test]
    fn invalid_utf8_outside_selection_is_preserved() {
        let mut original = b"good\n".to_vec();
        original.extend_from_slice(b"bad-\xFF-\xFE-bytes\r\n");
        original.extend_from_slice(b"tail");
        let mut document = Document::from_bytes_with_encoding(
            std::env::temp_dir().join("invalid.txt"),
            original.clone(),
            TextEncoding::utf8(),
        )
        .unwrap();
        assert_eq!(document.line_text(1), "bad-\u{FFFD}-\u{FFFD}-bytes");
        let caret = Position::new(0, 4);
        document.apply_edit(Selection::caret(caret), "!").unwrap();
        assert_eq!(document.to_bytes()[..5], *b"good!");
        assert!(
            document
                .to_bytes()
                .ends_with(b"bad-\xFF-\xFE-bytes\r\ntail")
        );

        // Deleting a replacement-character cell removes exactly that raw byte.
        let caret = Position::new(1, 4);
        let end = Position::new(1, 5);
        document.apply_edit(Selection::new(caret, end), "").unwrap();
        assert_eq!(document.line_text(1), "bad--\u{FFFD}-bytes");
        assert!(document.to_bytes().ends_with(b"bad--\xFE-bytes\r\ntail"));
    }

    #[test]
    fn utf16_roundtrip_and_edit() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "hello\r\nworld\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let (_dir, _path, mut document) = open_bytes(bytes.clone());
        assert_eq!(document.encoding().kind(), EncodingKind::Utf16Le);
        assert_eq!(document.to_bytes(), bytes);
        document
            .apply_edit(Selection::caret(Position::new(0, 5)), "!")
            .unwrap();
        let expected = "hello!\r\nworld\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>();
        let mut expected_with_bom = vec![0xFF, 0xFE];
        expected_with_bom.extend_from_slice(&expected);
        assert_eq!(document.to_bytes(), expected_with_bom);
    }

    #[test]
    fn utf16be_roundtrip() {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "ab\r\ncd".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        let (_dir, _path, document) = open_bytes(bytes.clone());
        assert_eq!(document.encoding().kind(), EncodingKind::Utf16Be);
        assert_eq!(document.to_bytes(), bytes);
    }

    #[test]
    fn legacy_encoding_is_preserved_and_input_is_validated() {
        let text = "第一行\r\n第二行 with ascii\n第三行";
        let (bytes, _, had_errors) = GBK.encode(text);
        assert!(!had_errors);
        let original = bytes.into_owned();
        let mut document = Document::from_bytes_with_encoding(
            std::env::temp_dir().join("gbk.txt"),
            original.clone(),
            TextEncoding::for_encoding(GBK),
        )
        .unwrap();
        assert_eq!(document.line_text(0), "第一行");
        assert_eq!(document.to_bytes(), original);

        let caret = Position::new(0, 3);
        document
            .apply_edit(Selection::caret(caret), "新增")
            .unwrap();
        let text = document.line_text(0);
        assert_eq!(text, "第一行新增");
        assert_eq!(document.line_text(2), "第三行");

        let emoji = document.apply_edit(Selection::caret(Position::new(0, 0)), "\u{1F600}");
        assert!(emoji.is_err());
        assert!(!document.to_bytes().windows(2).any(|w| w == b"\xF0\x9F"));
    }

    #[test]
    fn legacy_unicode_rejected_without_any_document_mutation() {
        let mut document = Document::from_bytes_with_encoding(
            std::env::temp_dir().join("gbk2.txt"),
            GBK.encode("中文").0.into_owned(),
            TextEncoding::for_encoding(GBK),
        )
        .unwrap();
        let before = document.to_bytes();
        let revision = document.revision();
        let result = document.apply_edit(Selection::caret(Position::new(0, 1)), "😀");
        assert!(result.is_err());
        assert_eq!(document.to_bytes(), before);
        assert_eq!(document.revision(), revision);
        assert!(!document.can_undo());
    }

    #[test]
    fn missing_path_and_parents_are_recreated_and_renamed_file_is_left_alone() {
        let dir = docs_dir();
        let subdir = dir.path().join("nested");
        fs::create_dir_all(&subdir).unwrap();
        let path = subdir.join("note.txt");
        fs::write(&path, b"original").unwrap();
        let mut document = Document::open(&path).unwrap();
        document
            .apply_edit(
                Selection::new(Position::new(0, 0), Position::new(0, 8)),
                "new content",
            )
            .unwrap();

        let renamed = dir.path().join("renamed.txt");
        fs::rename(&path, &renamed).unwrap();
        fs::remove_dir_all(&subdir).unwrap();
        document.save().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new content");
        assert_eq!(fs::read(&renamed).unwrap(), b"original");
    }

    #[test]
    fn existing_different_file_at_bound_path_is_overwritten() {
        let dir = docs_dir();
        let path = dir.path().join("file.txt");
        fs::write(&path, b"old").unwrap();
        let mut document = Document::open(&path).unwrap();
        document
            .apply_edit(Selection::caret(Position::new(0, 3)), " + new")
            .unwrap();
        fs::write(&path, b"someone else's file").unwrap();
        document.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old + new");
    }

    #[test]
    fn undo_redo_restores_exact_bytes() {
        let original = b"abc\r\ndef\nghi".to_vec();
        let (_dir, _path, mut document) = open_bytes(original.clone());
        let edit = Selection::new(Position::new(0, 2), Position::new(1, 1));
        document.apply_edit(edit, "Z\nQ").unwrap();
        let edited = document.to_bytes();
        assert_ne!(edited, original);
        let restored = document.undo().unwrap();
        assert_eq!(restored, edit);
        assert_eq!(document.to_bytes(), original);
        let redone = document.redo().unwrap();
        assert_eq!(redone, Selection::caret(Position::new(1, 1)));
        assert_eq!(document.to_bytes(), edited);
    }

    #[test]
    fn new_lines_follow_first_observed_eol() {
        let (_dir, _path, mut document) = open_bytes(b"a\r\nb".to_vec());
        document
            .apply_edit(Selection::caret(Position::new(0, 1)), "1\n2")
            .unwrap();
        assert_eq!(document.to_bytes(), b"a1\r\n2\r\nb");
    }

    #[test]
    fn save_reports_directory_target_as_error() {
        let dir = docs_dir();
        let target = dir.path().join("folder");
        fs::create_dir_all(&target).unwrap();
        let mut document = Document::from_bytes(target.clone(), b"x".to_vec()).unwrap();
        let error = document.save().unwrap_err();
        assert_eq!(error.path, target);
    }
}
