use glyphon::FontSystem;
use glyphon::cosmic_text::{Attrs, Buffer, Color, Family, LineEnding, Metrics, Shaping, Wrap};
use glyphon::cosmic_text::{AttrsList, BufferLine};

pub fn attrs(family: &'static str, color: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(family)).color(color)
}

/// A label that is shaped the moment it is built: one line, no wrapping, no bounds.
pub fn make_label(
    font_system: &mut FontSystem,
    attr: &Attrs<'static>,
    font_size: f32,
    text: &str,
) -> Buffer {
    let line_height = (font_size * 1.35).ceil();
    let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
    buffer.set_wrap(Wrap::None);
    buffer.set_size(None, None);
    buffer.set_text(text, attr, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    buffer
}

/// How much of a line a buffer actually took, for placing it against an edge.
pub fn measure(buffer: &Buffer) -> (f32, f32) {
    match buffer.layout_runs().next() {
        Some(run) => (run.line_w, run.line_height),
        None => (0.0, 0.0),
    }
}

pub fn buffer_line(text: &str, attrs: &Attrs<'static>) -> BufferLine {
    BufferLine::new(
        text.to_string(),
        LineEnding::Lf,
        AttrsList::new(attrs),
        Shaping::Advanced,
    )
}

pub fn char_to_byte(text: &str, ch: usize) -> usize {
    if ch == 0 {
        return 0;
    }
    text.char_indices()
        .nth(ch)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub fn byte_to_char(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[..byte].chars().count()
}

pub fn force_relayout(buffer: &mut Buffer) {
    let (width, height) = buffer.size();
    buffer.set_size(None, None);
    buffer.set_size(width, height);
}
