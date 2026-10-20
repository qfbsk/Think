use std::fmt;

use encoding_rs::{CoderResult, EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingKind {
    Utf8,
    Utf16Le,
    Utf16Be,
    Other,
}

#[derive(Clone, Debug)]
pub struct DetectedEncoding {
    pub encoding: TextEncoding,
}

#[derive(Clone, Debug)]
pub struct TextEncoding {
    encoding: &'static Encoding,
    kind: EncodingKind,
    bom: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unencodable {
    pub ch: char,
}

impl fmt::Display for Unencodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "character U+{:04X} is not representable", self.ch as u32)
    }
}

impl std::error::Error for Unencodable {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedEncoding(pub &'static str);

impl fmt::Display for UnsupportedEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported encoding: {}", self.0)
    }
}

impl std::error::Error for UnsupportedEncoding {}

impl TextEncoding {
    pub fn detect(bytes: &[u8]) -> Result<DetectedEncoding, UnsupportedEncoding> {
        // encoding_rs does not implement UTF-32. Reject it instead of silently
        // decoding UTF-32 bytes as UTF-16.
        if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00])
            || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
        {
            return Err(UnsupportedEncoding("UTF-32"));
        }
        if let Some((encoding, bom_length)) = Encoding::for_bom(bytes) {
            let kind = kind_of(encoding);
            return Ok(DetectedEncoding {
                encoding: TextEncoding {
                    encoding,
                    kind,
                    bom: bytes[..bom_length].to_vec(),
                },
            });
        }
        if std::str::from_utf8(bytes).is_ok() {
            return Ok(DetectedEncoding {
                encoding: TextEncoding {
                    encoding: UTF_8,
                    kind: EncodingKind::Utf8,
                    bom: Vec::new(),
                },
            });
        }
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(bytes, true);
        let encoding = detector.guess(None, true);
        if encoding == encoding_rs::REPLACEMENT {
            return Err(UnsupportedEncoding("replacement"));
        }
        Ok(DetectedEncoding {
            encoding: TextEncoding {
                encoding,
                kind: kind_of(encoding),
                bom: Vec::new(),
            },
        })
    }

    pub fn utf8() -> Self {
        Self {
            encoding: UTF_8,
            kind: EncodingKind::Utf8,
            bom: Vec::new(),
        }
    }

    pub fn for_encoding(encoding: &'static Encoding) -> Self {
        Self {
            encoding,
            kind: kind_of(encoding),
            bom: Vec::new(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.encoding.name()
    }

    pub fn kind(&self) -> EncodingKind {
        self.kind
    }

    pub fn bom(&self) -> &[u8] {
        &self.bom
    }

    pub fn has_bom(&self) -> bool {
        !self.bom.is_empty()
    }

    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    pub fn decode_lossy(&self, bytes: &[u8]) -> String {
        match self.kind {
            EncodingKind::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            EncodingKind::Utf16Le | EncodingKind::Utf16Be | EncodingKind::Other => self
                .encoding
                .decode_without_bom_handling(bytes)
                .0
                .into_owned(),
        }
    }

    /// Encodes new text exactly, rejecting anything the tab encoding cannot
    /// represent. Never falls back to HTML references or replacement bytes.
    pub fn encode_checked(&self, text: &str) -> Result<Vec<u8>, Unencodable> {
        match self.kind {
            EncodingKind::Utf8 => Ok(text.as_bytes().to_vec()),
            EncodingKind::Utf16Le => Ok(text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>()),
            EncodingKind::Utf16Be => Ok(text
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<u8>>()),
            EncodingKind::Other => {
                let first_capacity = text
                    .len()
                    .checked_mul(4)
                    .and_then(|n| n.checked_add(64))
                    .ok_or(Unencodable {
                        ch: text.chars().next().unwrap_or('\0'),
                    })?;
                for capacity in [first_capacity, first_capacity.saturating_mul(4)] {
                    let mut out = vec![0u8; capacity];
                    let mut encoder = self.encoding.new_encoder();
                    let (result, read, written) =
                        encoder.encode_from_utf8_without_replacement(text, &mut out, true);
                    match result {
                        EncoderResult::InputEmpty if read == text.len() => {
                            out.truncate(written);
                            return Ok(out);
                        }
                        EncoderResult::OutputFull => continue,
                        EncoderResult::Unmappable(ch) => return Err(Unencodable { ch }),
                        EncoderResult::InputEmpty => continue,
                    }
                }
                Err(Unencodable {
                    ch: find_unencodable(self.encoding, text).unwrap_or('\0'),
                })
            }
        }
    }

    pub fn default_eol(&self) -> Vec<u8> {
        match self.kind {
            EncodingKind::Utf16Le => vec![b'\n', 0],
            EncodingKind::Utf16Be => vec![0, b'\n'],
            _ => vec![b'\n'],
        }
    }

    /// Streaming decoder used to map each displayed character back to the raw
    /// bytes it came from. Feeding one byte at a time keeps the provenance
    /// exact; this path is only needed for lines whose encoding is not UTF-8.
    pub(crate) fn decode_with_spans(&self, raw: &[u8]) -> (String, Vec<(usize, usize)>) {
        let mut decoder = self.encoding.new_decoder_without_bom_handling();
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut buffer = [0u8; 16];
        let mut raw_pos = 0usize;
        let mut pending_start = 0usize;
        let mut had_errors = false;

        while raw_pos < raw.len() {
            let (_, read, written, errors) =
                decoder.decode_to_utf8(&raw[raw_pos..raw_pos + 1], &mut buffer, false);
            had_errors |= errors;
            raw_pos += read;
            if written > 0 {
                let chunk = std::str::from_utf8(&buffer[..written])
                    .expect("encoding_rs emitted invalid UTF-8");
                append_chunk(&mut text, &mut spans, chunk, pending_start, raw_pos);
                pending_start = raw_pos;
            }
            if read == 0 && written == 0 {
                // A single input byte always either advances the decoder or
                // emits a scalar. Bail out instead of spinning if a stateful
                // decoder ever violates that.
                break;
            }
        }

        loop {
            let (result, read, written, errors) = decoder.decode_to_utf8(&[], &mut buffer, true);
            debug_assert_eq!(read, 0);
            had_errors |= errors;
            if written > 0 {
                let chunk = std::str::from_utf8(&buffer[..written])
                    .expect("encoding_rs emitted invalid UTF-8");
                append_chunk(&mut text, &mut spans, chunk, pending_start, raw.len());
                pending_start = raw.len();
            }
            if result == CoderResult::InputEmpty {
                break;
            }
        }

        if pending_start < raw.len() {
            text.push('\u{FFFD}');
            spans.push((pending_start, raw.len()));
        }

        // Keep the displayed text identical to the fast lossy path. If they
        // diverge, the difference is limited to replacement grouping; use the
        // lossy text only when the character counts still agree.
        let lossy = self.decode_lossy(raw);
        if had_errors && text != lossy && text.chars().count() == lossy.chars().count() {
            text = lossy;
        }

        (text, spans)
    }
}

fn append_chunk(
    text: &mut String,
    spans: &mut Vec<(usize, usize)>,
    chunk: &str,
    span_start: usize,
    span_end: usize,
) {
    let mut first = true;
    for ch in chunk.chars() {
        text.push(ch);
        if first {
            spans.push((span_start, span_end));
            first = false;
        } else {
            spans.push((span_end, span_end));
        }
    }
}

fn find_unencodable(encoding: &'static Encoding, text: &str) -> Option<char> {
    text.chars().find(|ch| {
        let mut buf = [0u8; 16];
        let mut encoder = encoding.new_encoder();
        matches!(
            encoder
                .encode_from_utf8_without_replacement(&ch.to_string(), &mut buf, true)
                .0,
            EncoderResult::Unmappable(_)
        )
    })
}

fn kind_of(encoding: &'static Encoding) -> EncodingKind {
    if encoding == UTF_8 {
        EncodingKind::Utf8
    } else if encoding == UTF_16LE {
        EncodingKind::Utf16Le
    } else if encoding == UTF_16BE {
        EncodingKind::Utf16Be
    } else {
        EncodingKind::Other
    }
}
