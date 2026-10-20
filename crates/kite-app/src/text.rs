use glyphon::cosmic_text::{Attrs, Buffer, Color, Family, LineEnding, Shaping};
use glyphon::cosmic_text::{AttrsList, BufferLine};

pub fn attrs(family: &'static str, color: Color) -> Attrs<'static> {
    Attrs::new().family(Family::Name(family)).color(color)
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
