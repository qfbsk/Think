//! Byte-faithful document, encoding and path primitives for kite.
//!
//! The document keeps untouched file bytes in their original location. A line
//! is a list of pieces: ranges into the original file, or bytes produced by an
//! edit. Saving walks the pieces and writes every untouched byte verbatim, so
//! preservation is a property of the representation rather than a save-time
//! repair step.

mod document;
mod encoding;
mod path;

pub use document::{Change, Document, DocumentError, Line, Position, SaveError, Selection};
pub use encoding::{
    DetectedEncoding, EncodingKind, TextEncoding, Unencodable, UnsupportedEncoding,
};
pub use path::{FileKey, absolute_bound_path, display_path, file_key, identity, plain_path, start_dir};
