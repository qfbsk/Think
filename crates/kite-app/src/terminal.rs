//! The terminal core: one screen grid, fed by a real pseudo console.
//!
//! A console program attached to a ConPTY believes it is talking to a
//! terminal: its prompt reaches the screen the moment it writes it, cursor
//! addressing and colours mean what they say, and full screen programs
//! repaint correctly. Everything the shell tab shows ends up on this grid.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite, Options, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};
use glyphon::cosmic_text::Color;
use polling::{Event as PollEvent, Events, PollMode, Poller};

/// Rows kept above the viewport, matching the shell's scrollback budget.
const SCROLLBACK: usize = 4000;
const READ_BUFFER: usize = 64 * 1024;
const PTY_KEY: usize = 0;
/// The short timeout is only there so a keystroke cannot sit in the command
/// queue until the next read.
const TICK: Duration = Duration::from_millis(4);

/// The sixteen ANSI slots in Catppuccin Mocha, the palette the rest of kite
/// already speaks. Bright slots reuse their hue; the theme has one green.
const ANSI: [u32; 16] = [
    0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70,
    0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
];

/// A stretch of one row that shares colour and weight. The renderer wants
/// runs; the grid speaks cells.
pub struct Run {
    pub text: String,
    pub color: Option<Color>,
    pub bold: bool,
}

fn pack(value: u32) -> Color {
    Color::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// The 256 colour slots: 16 palette entries, then a 6x6x6 cube, then a gray
/// ramp.
fn indexed(index: u8) -> u32 {
    const STEPS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let value = u32::from(index) - 16;
            (STEPS[(value / 36) as usize] << 16)
                | (STEPS[(value / 6 % 6) as usize] << 8)
                | STEPS[(value % 6) as usize]
        }
        _ => {
            let level = u32::from(index) - 232;
            let gray = 8 + level * 10;
            (gray << 16) | (gray << 8) | gray
        }
    }
}

/// `None` means "the theme's own text colour", so an unstyled screen keeps
/// following `theme.toml`.
fn resolve(color: AnsiColor) -> Option<Color> {
    let named = |slot: usize| Some(pack(ANSI[slot]));
    match color {
        AnsiColor::Named(NamedColor::Black) => named(0),
        AnsiColor::Named(NamedColor::Red) => named(1),
        AnsiColor::Named(NamedColor::Green) => named(2),
        AnsiColor::Named(NamedColor::Yellow) => named(3),
        AnsiColor::Named(NamedColor::Blue) => named(4),
        AnsiColor::Named(NamedColor::Magenta) => named(5),
        AnsiColor::Named(NamedColor::Cyan) => named(6),
        AnsiColor::Named(NamedColor::White) => named(7),
        AnsiColor::Named(NamedColor::BrightBlack) => named(8),
        AnsiColor::Named(NamedColor::BrightRed) => named(9),
        AnsiColor::Named(NamedColor::BrightGreen) => named(10),
        AnsiColor::Named(NamedColor::BrightYellow) => named(11),
        AnsiColor::Named(NamedColor::BrightBlue) => named(12),
        AnsiColor::Named(NamedColor::BrightMagenta) => named(13),
        AnsiColor::Named(NamedColor::BrightCyan) => named(14),
        AnsiColor::Named(NamedColor::BrightWhite) => named(15),
        AnsiColor::Indexed(index) => Some(pack(indexed(index))),
        AnsiColor::Spec(value) => Some(Color::rgb(value.r, value.g, value.b)),
        _ => None,
    }
}

/// Grid size, in cells. The terminal model derives every other bound from
/// these three numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    columns: usize,
    screen_lines: usize,
    total_lines: usize,
}

impl Size {
    fn new(columns: usize, screen_lines: usize) -> Self {
        let columns = columns.max(1);
        let screen_lines = screen_lines.max(1);
        Self {
            columns,
            screen_lines,
            total_lines: screen_lines,
        }
    }

    /// The history the model is allowed to keep when the grid is (re)built.
    fn with_history(&self) -> Self {
        Self {
            total_lines: self.screen_lines + SCROLLBACK,
            ..*self
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.total_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Side effects the grid asks the outside world to notice. Only `PtyWrite`
/// matters today: some programs ask the terminal to send bytes back to them.
#[derive(Clone, Default)]
pub(crate) struct Proxy {
    sink: Arc<Mutex<Option<Sender<TermEvent>>>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: TermEvent) {
        if let Some(sender) = self.sink.lock().unwrap().as_ref() {
            let _ = sender.send(event);
        }
    }
}

enum News {
    Bytes(Vec<u8>),
    Exited(Option<ExitStatus>),
}

enum Msg {
    Write(Vec<u8>),
    Resize(WindowSize),
    Close,
}

struct Job {
    news: Receiver<News>,
    writer: Sender<Msg>,
    worker: Option<thread::JoinHandle<()>>,
    pid: Option<u32>,
}

/// A mouse button, in grid cells, ready to be reported to the child.
#[derive(Clone, Copy, Debug)]
pub struct MouseReport {
    /// 0 left, 1 middle, 2 right; 64 wheel up, 65 wheel down.
    pub button: u8,
    pub press: bool,
    pub drag: bool,
    pub line: i32,
    pub column: usize,
}

/// What ConPTY is claiming about the screen, if anything: a description of the
/// buffer it holds, which is never the session the grid already has.
#[derive(Clone, Copy)]
enum Claim {
    /// The child has not written yet, so the screen it announced is the empty
    /// one it was just handed.
    Opening,
    /// The child gave the alternate screen back, and ConPTY is repainting the
    /// buffer it swapped to — the same empty one, now walked row by row.
    GivenBack,
}

impl Claim {
    /// How many bytes at the front of the stream say nothing the grid has to
    /// forget, or `None` at the first byte of real output.
    fn reads(self, bytes: &[u8]) -> Option<usize> {
        match self {
            Claim::Opening => announcement(bytes),
            Claim::GivenBack => repaint(bytes),
        }
    }
}

pub struct Terminal {
    term: Term<Proxy>,
    parser: Processor,
    size: Size,
    cell: (f32, f32),
    job: Option<Job>,
    exit: Option<Option<ExitStatus>>,
    events: Receiver<TermEvent>,
    /// Where the drag started and where it stands, in grid cells.
    selection: Option<(Point, Point)>,
    claim: Option<Claim>,
}

impl Terminal {
    pub fn new(panel: (f32, f32), cell: (f32, f32)) -> Self {
        let size = cells(panel, cell);
        let (sender, events) = channel();
        let proxy = Proxy {
            sink: Arc::new(Mutex::new(Some(sender))),
        };
        let mut config = Config::default();
        config.scrolling_history = SCROLLBACK;
        let term = Term::new(config, &size.with_history(), proxy);
        Terminal {
            term,
            parser: Processor::default(),
            size,
            cell,
            job: None,
            exit: None,
            events,
            selection: None,
            claim: None,
        }
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.job.as_ref().and_then(|job| job.pid)
    }

    /// One visible row, left to right, as text. Only the tests read the grid
    /// this way; the renderer walks the cells themselves for colours.
    #[cfg(test)]
    pub fn row_text(&self, line: i32) -> String {
        self.cells(line).into_iter().map(|(ch, _, _)| ch).collect()
    }

    /// The whole viewport joined, trailing blanks trimmed.
    #[cfg(test)]
    pub fn screen_text(&self) -> String {
        (0..self.size.screen_lines as i32)
            .map(|row| self.row_text(self.top_line() + row).trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The grid line sitting under the top of the screen. Zero means the view
    /// is at the bottom; negative means it is scrolled back into history.
    pub fn top_line(&self) -> i32 {
        -(self.term.grid().display_offset() as i32)
    }

    /// Whether the view is looking at history rather than the live bottom.
    pub fn scrolled(&self) -> bool {
        self.term.grid().display_offset() != 0
    }

    /// Rows of scrollback above the viewport.
    pub fn history(&self) -> i32 {
        self.term.grid().history_size() as i32
    }

    /// Move the view. Positive `lines` goes down towards the present.
    pub fn scroll_by(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(-lines));
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.scrolled() {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    /// Where the selection began and where it now ends, earliest point first.
    pub fn selection(&self) -> Option<(Point, Point)> {
        let (first, second) = self.selection?;
        Some(if first <= second {
            (first, second)
        } else {
            (second, first)
        })
    }

    pub fn select(&mut self, anchor: Point, cursor: Point) {
        self.selection = Some((anchor, cursor));
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The selected text, read out of the grid so wide characters, tabs and
    /// scrollback all come out the way they went in.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        (start != end).then(|| self.term.bounds_to_string(start, end))
    }

    /// Whether the child asked for the mouse. While it has, kite's own
    /// Whether the child asked for the mouse. While it has, kite's own
    /// selection and scrolling have to stand aside — but only while it is
    /// still there to hear about it, or the mode it left behind would swallow
    /// every wheel notch in the session.
    pub fn mouse_reporting(&self) -> bool {
        self.job.is_some() && self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Report a mouse button to the child.
    pub fn send_mouse(&mut self, report: MouseReport) {
        if !self.mouse_reporting() {
            return;
        }
        let mode = *self.term.mode();
        let bytes = mouse_bytes(mode, self.top_line(), self.size.screen_lines, report);
        if !bytes.is_empty() {
            self.send(&bytes);
        }
    }

    /// One visible row, cell by cell. A wide glyph is one entry and its
    /// trailing spacer is dropped, so the count can be under `columns`.
    pub fn cells(&self, line: i32) -> Vec<(char, Option<Color>, bool)> {
        let grid = self.term.grid();
        let row = Line(line);
        let mut cells = Vec::with_capacity(self.size.columns);
        let mut column = 0;
        while column < self.size.columns {
            let cell = &grid[Point::new(row, Column(column))];
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                cells.push((cell.c, resolve(cell.fg), cell.flags.contains(Flags::BOLD)));
            }
            column += if wide { 2 } else { 1 };
        }
        cells
    }

    pub fn runs(&self, line: i32) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for (ch, color, bold) in self.cells(line) {
            match runs.last_mut() {
                Some(run) if run.color == color && run.bold == bold => run.text.push(ch),
                _ => runs.push(Run {
                    text: ch.to_string(),
                    color,
                    bold,
                }),
            }
        }
        runs
    }

    /// Where the caret is, whether or not the child is showing one.
    pub fn cursor_point(&self) -> (usize, usize) {
        let content = self.term.renderable_content();
        (
            content.cursor.point.column.0,
            content.cursor.point.line.0.max(0) as usize,
        )
    }

    /// Where the child put its own cursor: column, and grid line (which is
    /// above the viewport whenever the view is scrolled down).
    pub fn cursor(&self) -> Option<(usize, i32)> {
        let content = self.term.renderable_content();
        (content.cursor.shape != CursorShape::Hidden)
            .then(|| (content.cursor.point.column.0, content.cursor.point.line.0))
    }

    pub fn rows(&self) -> usize {
        self.size.screen_lines
    }

    /// Which grid line the cursor is standing on, whether or not it shows one.
    /// After a program leaves the alternate screen this is where it had left
    /// it, which need not be the bottom row.
    pub fn cursor_line(&self) -> i32 {
        self.term.renderable_content().cursor.point.line.0
    }

    pub fn columns(&self) -> usize {
        self.size.columns
    }

    pub fn cell(&self) -> (f32, f32) {
        self.cell
    }

    /// Feed raw console output into the grid. Every byte the child writes
    /// comes through here, with nothing line buffered on the way.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let before = self.term.grid().history_size();
        self.parser.advance(&mut self.term, bytes);
        // Output that scrolls the screen moves the text a selection was made
        // on, so the highlight follows it. Once the scrollback cap is reached
        // the history stops growing and the selection stays where it is.
        let scrolled = self.term.grid().history_size().saturating_sub(before) as i32;
        if scrolled > 0 {
            if let Some((anchor, cursor)) = self.selection.take() {
                self.selection = Some((
                    Point::new(Line(anchor.line.0 - scrolled), anchor.column),
                    Point::new(Line(cursor.line.0 - scrolled), cursor.column),
                ));
            }
        }
    }

    /// Attach a pseudo console to `program`, so it gets a real screen.
    pub fn start(&mut self, program: &Path, args: &[String], cwd: &Path) -> io::Result<()> {
        self.close_job();
        let mut options = Options::default();
        options.shell = Some(Shell::new(
            program.display().to_string(),
            args.iter().map(String::to_string).collect(),
        ));
        options.working_directory = Some(cwd.to_path_buf());
        options.escape_args = true;
        let pty = tty::new(&options, self.window_size(), 0)?;
        let pid = pty.child_watcher().pid().map(|pid| pid.get());
        let (news_tx, news_rx) = channel();
        let (writer, cmds_rx) = channel();
        let worker = thread::Builder::new()
            .name("kite-pty".to_string())
            .spawn(move || pump(pty, news_tx, cmds_rx))
            .map_err(io::Error::other)?;
        self.claim = Some(Claim::Opening);
        self.selection = None;
        self.job = Some(Job {
            news: news_rx,
            writer,
            worker: Some(worker),
            pid,
        });
        Ok(())
    }

    /// Keystrokes, already encoded as VT, headed for the child's input. Typing
    /// always brings the view back to the bottom, as it does in a terminal.
    pub fn send(&mut self, bytes: &[u8]) {
        self.scroll_to_bottom();
        if let Some(job) = self.job.as_ref() {
            let _ = job.writer.send(Msg::Write(bytes.to_vec()));
        }
    }

    /// Re-fit the grid to the panel. The child is told as well, so a listing
    /// that wrapped at the old width re-wraps at the new one.
    pub fn resize(&mut self, panel: (f32, f32), cell: (f32, f32)) {
        let size = cells(panel, cell);
        if size == self.size && cell == self.cell {
            return;
        }
        self.cell = cell;
        self.size = size;
        self.term.resize(size.with_history());
        if let Some(job) = self.job.as_ref() {
            let _ = job.writer.send(Msg::Resize(self.window_size()));
        }
    }

    /// Take whatever the child wrote since last time. Returns true when the
    /// screen changed, so the caller can skip a redraw.
    pub fn poll(&mut self) -> bool {
        let mut news = Vec::new();
        if let Some(job) = self.job.as_ref() {
            while let Ok(message) = job.news.try_recv() {
                let last = matches!(message, News::Exited(_));
                news.push(message);
                if last {
                    break;
                }
            }
        }
        let mut changed = false;
        for message in news {
            match message {
                News::Bytes(bytes) => {
                    self.feed_pty(&bytes);
                    changed = true;
                }
                News::Exited(status) => {
                    self.exit = Some(status);
                    self.close_job();
                    changed = true;
                }
            }
        }
        while let Ok(event) = self.events.try_recv() {
            if let TermEvent::PtyWrite(text) = event {
                self.send(text.as_bytes());
            }
        }
        changed
    }

    /// The exit status of the child that just went away, once. `None` inside
    /// `Some` means it died without reporting one.
    pub fn take_exit(&mut self) -> Option<Option<ExitStatus>> {
        self.exit.take()
    }

    /// Stop the child and let go of the screen.
    pub fn kill(&mut self) {
        self.close_job();
    }

    /// Bytes from the pseudo console. ConPTY opens by announcing an empty
    /// screen — `ESC[2J` then `ESC[H` — which is true of the buffer it just
    /// made and false of the grid it is being attached to, which already holds
    /// the session. That part is dropped. The modes it sets on the way there
    /// are the child's own requests, alternate screen included, and are fed
    /// through: swallowing `ESC[?1049h` would cost the session its history.
    ///
    /// A child that hands the alternate screen back announces the same empty
    /// buffer a second time, because the buffer it swapped to is the one it was
    /// handed. It gets the same answer, walked row by row.
    fn feed_pty(&mut self, bytes: &[u8]) {
        let mut start = 0;
        if self.claim.is_none() {
            // A child hands the screen back in the middle of a chunk as often
            // as at its edge, and everything after that is ConPTY painting the
            // buffer it swapped to. Split before that paint begins.
            if let Some(end) = handback(bytes) {
                self.feed(&bytes[..end]);
                start = end;
                self.claim = Some(Claim::GivenBack);
            }
        }
        if let Some(claim) = self.claim {
            while start < bytes.len() {
                let Some(len) = claim.reads(&bytes[start..]) else {
                    break;
                };
                let piece = &bytes[start..start + len];
                if piece.starts_with(b"\x1b[?") {
                    self.parser.advance(&mut self.term, piece);
                }
                start += len;
            }
            if start < bytes.len() {
                self.claim = None;
            }
        }
        let held_screen = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.feed(&bytes[start..]);
        if held_screen && !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.claim = Some(Claim::GivenBack);
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.screen_lines as u16,
            num_cols: self.size.columns as u16,
            cell_width: self.cell.0.max(1.0) as u16,
            cell_height: self.cell.1.max(1.0) as u16,
        }
    }

    fn close_job(&mut self) {
        if let Some(mut job) = self.job.take() {
            let _ = job.writer.send(Msg::Close);
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
        }
        // A child that dies inside the alternate screen would take the session
        // down with it. Once it is gone the screen belongs to the session.
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.parser.advance(&mut self.term, b"\x1b[?1049l");
        }
    }
}

/// The bytes a mouse report turns into, or nothing when the child never asked
/// for the mouse, or the point is outside its screen.
fn mouse_bytes(mode: TermMode, top: i32, screen_lines: usize, report: MouseReport) -> Vec<u8> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return Vec::new();
    }
    let row = report.line - top;
    if row < 0 || row >= screen_lines as i32 {
        return Vec::new();
    }
    let (column, row) = (report.column + 1, row as usize + 1);
    let button = report.button as usize + if report.drag { 32 } else { 0 };
    if mode.contains(TermMode::SGR_MOUSE) {
        let tail = if report.press { 'M' } else { 'm' };
        format!("\x1b[{button};{column};{row}{tail}").into_bytes()
    } else if report.press {
        // The X10 encoding packs each coordinate into a printable byte and
        // stops counting at 223, so a far column reads as 223 to the child.
        let pack = |value: usize| (value.min(222) + 32) as u8;
        vec![
            0x1b,
            b'[',
            b'M',
            b'@' + button as u8,
            pack(column),
            pack(row),
        ]
    } else {
        // X10 has no release event to send.
        Vec::new()
    }
}

/// The length of a leading screen-reset announcement, when there is one.
fn announcement(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(b"\x1b[" as &[u8])?;
    if rest.first() == Some(&b'?') {
        let digits = rest[1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        return match rest.get(1 + digits)? {
            b'h' | b'l' if digits > 0 => Some(4 + digits),
            _ => None,
        };
    }
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    match (digits, rest.get(digits)?) {
        (0, b'm') | (0, b'H') | (0, b'f') => Some(3),
        (1, b'J') if rest[0] == b'2' => Some(4),
        _ => None,
    }
}

/// Just past the point where a child asks for the main screen again, if it does
/// so inside these bytes.
fn handback(bytes: &[u8]) -> Option<usize> {
    [b"\x1b[?1049l".as_slice(), b"\x1b[?1047l", b"\x1b[?47l"]
        .into_iter()
        .filter_map(|needle| {
            bytes
                .windows(needle.len())
                .position(|window| window == needle)
                .map(|at| at + needle.len())
        })
        .min()
}

/// The whole of a buffer repaint: erasing, moving about and retitling, which is
/// how ConPTY walks a screen row by row on its way to whatever it holds. An
/// empty buffer amounts to nothing but this, so the session can let it pass and
/// keep its own rows. The first character of real output ends the run.
fn repaint(bytes: &[u8]) -> Option<usize> {
    if matches!(bytes.first(), Some(b'\r') | Some(b'\n')) {
        return Some(1);
    }
    if let Some(len) = announcement(bytes) {
        return Some(len);
    }
    if let Some(body) = bytes.strip_prefix(b"\x1b]" as &[u8]) {
        return body.iter().position(|byte| *byte == 7).map(|end| end + 3);
    }
    let params = bytes
        .strip_prefix(b"\x1b[" as &[u8])?
        .iter()
        .take_while(|byte| byte.is_ascii_digit() || **byte == b';')
        .count();
    match *bytes.get(2 + params)? {
        b'H' | b'f' | b'K' | b'J' | b'C' | b't' => Some(3 + params),
        _ => None,
    }
}

/// Push bytes into the console's input pipe. The writer is non-blocking and
/// may take the bytes one go at a time; keystrokes are short, so waiting here
/// is cheaper than building a queue.
fn write_all(pty: &mut tty::Pty, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    let deadline = Instant::now() + Duration::from_secs(1);
    while offset < bytes.len() {
        match pty.writer().write(&bytes[offset..]) {
            // This writer reports `Ok(0)` when the pipe is not ready, which is
            // its version of would-block; giving up would drop the keystroke.
            Ok(0) => {
                if Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn cells(panel: (f32, f32), cell: (f32, f32)) -> Size {
    let columns = (panel.0 / cell.0.max(f32::EPSILON)).floor().max(1.0);
    let screen_lines = (panel.1 / cell.1.max(f32::EPSILON)).floor().max(1.0);
    Size::new(columns as usize, screen_lines as usize)
}

/// The pty lives on this thread: the child's screen is read here, and its
/// input is written here, so neither end can ever block the UI.
fn pump(mut pty: tty::Pty, news: Sender<News>, cmds: Receiver<Msg>) {
    let Ok(poller) = Poller::new() else { return };
    let poller = Arc::new(poller);
    let interest = PollEvent::readable(PTY_KEY);
    // SAFETY: the pty and the poller are both owned by this thread and are
    // dropped together with its stack frame, so the registration cannot outlive
    // what it refers to.
    let watching = unsafe { pty.register(&poller, interest, PollMode::Oneshot) }.is_ok();
    let mut events = Events::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        if watching {
            let _ = poller.wait(&mut events, Some(TICK));
            events.clear();
        }
        let mut drained = false;
        loop {
            match pty.reader().read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    drained = true;
                    if news.send(News::Bytes(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if watching && drained {
            let _ = pty.reregister(&poller, interest, PollMode::Oneshot);
        }
        while let Ok(command) = cmds.try_recv() {
            match command {
                Msg::Write(bytes) => {
                    let _ = write_all(&mut pty, &bytes);
                }
                Msg::Resize(size) => pty.on_resize(size),
                Msg::Close => return,
            }
        }
        if let Some(ChildEvent::Exited(status)) = pty.next_child_event() {
            let _ = news.send(News::Exited(status));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    fn screen(content: &[u8]) -> Terminal {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        terminal.feed(content);
        terminal
    }

    fn cell(terminal: &Terminal, line: usize, column: usize) -> char {
        terminal.term.grid()[Point::new(Line(line as i32), Column(column))].c
    }

    fn row(terminal: &Terminal, line: usize) -> String {
        (0..terminal.size.columns)
            .map(|column| cell(terminal, line, column))
            .collect::<String>()
    }

    #[test]
    fn an_unterminated_prompt_lands_on_the_grid_at_once() {
        // The exact case the line model could not do: `printf("input n: ")`
        // with no newline, waiting on a keyboard that has not answered yet.
        let terminal = screen(b"input n: ");
        assert_eq!(row(&terminal, 0).trim_end(), "input n:");
        assert_eq!(terminal.term.renderable_content().cursor.point.column.0, 9);
    }

    #[test]
    fn cursor_addressing_and_line_erase_reach_the_right_cells() {
        let terminal = screen(b"abc\r\ndefg\r\nhij\r\n\x1b[2;1H\x1b[K");
        assert_eq!(row(&terminal, 0).trim_end(), "abc");
        assert_eq!(row(&terminal, 1).trim_end(), "");
        assert_eq!(row(&terminal, 2).trim_end(), "hij");
    }

    #[test]
    fn colours_are_cells_not_text() {
        let terminal = screen(b"\x1b[31mred\x1b[39mplain");
        let grid = terminal.term.grid();
        let red = &grid[Point::new(Line(0), Column(0))];
        assert_eq!(red.fg, Color::Named(NamedColor::Red));
        assert_eq!(red.c, 'r');
        assert_eq!(cell(&terminal, 0, 3), 'p');
        assert_eq!(
            grid[Point::new(Line(0), Column(3))].fg,
            Color::Named(NamedColor::Foreground)
        );
    }

    #[test]
    fn the_grid_fits_the_panel() {
        let terminal = screen(b"");
        assert_eq!(terminal.size.columns, 100);
        assert_eq!(terminal.size.screen_lines, 30);
        let narrow = screen(b"01234567890123456789012345678901234567890123456789");
        assert_eq!(narrow.size.columns, 100);
        assert_eq!(row(&narrow, 0).trim_end().len(), 50);
    }

    #[test]
    fn a_real_console_program_gets_a_real_screen() {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        terminal.feed(b"the session so far\r\n");
        let args = vec!["/c".to_string(), "echo hello-pty".to_string()];
        terminal
            .start(
                Path::new("cmd.exe"),
                &args,
                &std::env::current_dir().unwrap(),
            )
            .expect("the pseudo console should start");
        let mut screen = String::new();
        for _ in 0..400 {
            terminal.poll();
            if !terminal.running() {
                screen = terminal.screen_text();
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(screen.contains("hello-pty"), "the grid held: {screen:?}");
        assert!(
            screen.contains("the session so far"),
            "ConPTY's opening reset wiped the session: {screen:?}"
        );
    }

    #[test]
    fn the_opening_keeps_the_alternate_screen_the_child_asked_for() {
        let mut terminal = screen(b"the listing\r\n");
        terminal.claim = Some(Claim::Opening);
        terminal.feed_pty(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        assert!(
            terminal.term.mode().contains(TermMode::ALT_SCREEN),
            "the mode the child set is not an announcement"
        );
        let screen = terminal.screen_text();
        assert!(screen.contains("full screen"), "{screen}");
        assert!(!screen.contains("the listing"), "it has its own screen");

        terminal.claim = Some(Claim::Opening);
        terminal.feed_pty(b"\x1b[?1049l");
        assert!(
            terminal.screen_text().contains("the listing"),
            "the session comes back when the program leaves"
        );
    }

    #[test]
    fn the_screen_a_child_gives_back_is_not_a_blank_one() {
        let mut terminal = screen(b"the listing\r\n");
        terminal.feed_pty(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        assert!(terminal.screen_text().contains("full screen"));

        // What ConPTY really sends when vim hands the screen back: the swap, a
        // title pop, then its own empty buffer painted row by row. Feeding
        // that to the grid erases the session and homes the cursor.
        terminal.feed_pty(b"\r\x1b[K\x1b[?25l\x1b[140C\x1b[?1049l");
        terminal.feed_pty(b"\x1b[23;0;0t");
        terminal.feed_pty(b"\x1b[H\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\x1b[H\x1b[?25h");
        let screen = terminal.screen_text();
        assert!(
            screen.contains("the listing"),
            "the repaint of the child's buffer wiped the session:\n{screen}"
        );
        assert_eq!(
            terminal.cursor_line(),
            1,
            "the cursor stays where the session left it"
        );
    }

    #[test]
    fn a_child_that_writes_after_giving_the_screen_back_is_heard() {
        let mut terminal = screen(b"the listing\r\n");
        terminal.feed_pty(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        terminal.feed_pty(b"\x1b[?1049l\x1b[H\x1b[K\r\n");
        terminal.feed_pty(b"the choice\r\n");
        let screen = terminal.screen_text();
        assert!(
            screen.contains("the choice") && screen.contains("the listing"),
            "{screen}"
        );
    }

    #[test]
    fn a_child_that_dies_in_the_alternate_screen_leaves_it_with_the_session() {
        let mut terminal = screen(b"the listing\r\n");
        terminal.feed_pty(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        terminal.close_job();
        assert!(
            terminal.screen_text().contains("the listing"),
            "the session was still behind the screen the child never left"
        );
    }

    #[test]
    fn only_the_opening_announcement_reads_as_a_reset() {
        assert_eq!(announcement(b"\x1b[?9001h\x1b[2J"), Some(8));
        assert_eq!(announcement(b"\x1b[?25l\x1b[m"), Some(6));
        assert_eq!(announcement(b"\x1b[H\x1b[2J"), Some(3));
        assert_eq!(
            announcement(b"\x1b[1;31mred"),
            None,
            "a colour is not a reset"
        );
        assert_eq!(
            announcement(b"\r\nthe child speaks"),
            None,
            "a blank line is the child's own"
        );
        assert_eq!(announcement(b"spoken"), None);
        assert_eq!(announcement(b""), None);
    }

    #[test]
    fn a_repaint_is_all_the_empty_buffer_ever_adds_up_to() {
        assert_eq!(repaint(b"\x1b[23;0;0t"), Some(9));
        assert_eq!(repaint(b"\x1b[H\x1b[K\r\n"), Some(3));
        assert_eq!(repaint(b"\x1b[31;130H"), Some(9));
        assert_eq!(repaint(b"\x1b]0;C:\\vim.exe\x07"), Some(15));
        assert_eq!(repaint(b"\x1b[?1049l"), Some(8));
        assert_eq!(
            repaint(b"\x1b[34mcolour"),
            None,
            "a colour belongs to the text that follows"
        );
        assert_eq!(repaint(b"the choice"), None);
    }

    #[test]
    fn resizing_rewraps_the_grid_and_shows_the_cursor() {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        terminal.feed(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(row(&terminal, 1).trim_end().len(), 0);
        terminal.resize((400.0, 240.0), (8.0, 16.0));
        assert_eq!(terminal.size.columns, 50);
        assert_eq!(terminal.size.screen_lines, 15);
        // A grid that has content always has a drawable cursor.
        assert_ne!(
            terminal.term.renderable_content().cursor.shape,
            alacritty_terminal::vte::ansi::CursorShape::Hidden
        );
    }

    #[test]
    fn the_wheel_walks_scrollback_and_back() {
        let mut terminal = Terminal::new((800.0, 160.0), (8.0, 16.0));
        assert_eq!(terminal.rows(), 10);
        for line in 1..=30 {
            terminal.feed(format!("line {line}\r\n").as_bytes());
        }
        assert_eq!(terminal.history(), 21);
        assert_eq!(terminal.top_line(), 0);
        assert!(terminal.screen_text().contains("line 30"));

        terminal.scroll_by(-5);
        assert!(terminal.scrolled());
        assert_eq!(terminal.top_line(), -5);
        assert!(terminal.screen_text().contains("line 25"));
        assert!(!terminal.screen_text().contains("line 30"));

        terminal.scroll_by(5);
        assert!(!terminal.scrolled());
        assert!(terminal.screen_text().contains("line 30"));
    }

    #[test]
    fn a_selection_follows_the_text_it_was_made_on() {
        let mut terminal = Terminal::new((800.0, 160.0), (8.0, 16.0));
        for row in 1..=9 {
            terminal.feed(format!("row {row}\r\n").as_bytes());
        }
        assert_eq!(terminal.history(), 0, "nine lines still fit the screen");
        terminal.select(
            Point::new(Line(0), Column(0)),
            Point::new(Line(0), Column(3)),
        );
        assert_eq!(terminal.selected_text().as_deref(), Some("row "));

        terminal.feed(b"row 10\r\n");
        assert_eq!(terminal.history(), 1);
        assert_eq!(
            terminal.selection().map(|(start, _)| start.line.0),
            Some(-1),
            "the highlight moved up with its line"
        );
        assert_eq!(
            terminal.selected_text().as_deref(),
            Some("row "),
            "and still names the same text"
        );
    }

    #[test]
    fn mouse_reports_are_encoded_the_way_the_child_asked() {
        let report = MouseReport {
            button: 0,
            press: true,
            drag: false,
            line: 2,
            column: 7,
        };
        assert!(
            mouse_bytes(TermMode::empty(), 0, 10, report).is_empty(),
            "no mode, no report"
        );

        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_bytes(sgr, 0, 10, report), b"\x1b[0;8;3M");
        assert_eq!(
            mouse_bytes(
                sgr,
                0,
                10,
                MouseReport {
                    drag: true,
                    ..report
                }
            ),
            b"\x1b[32;8;3M"
        );
        assert_eq!(
            mouse_bytes(
                sgr,
                0,
                10,
                MouseReport {
                    press: false,
                    ..report
                }
            ),
            b"\x1b[0;8;3m"
        );
        assert_eq!(mouse_bytes(sgr, -5, 10, report), b"\x1b[0;8;8M");
        assert!(
            mouse_bytes(sgr, -5, 10, MouseReport { line: -6, ..report }).is_empty(),
            "a row above the screen is not the child's"
        );

        // X10 packs each coordinate into a printable byte and has no release.
        let x10 = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            mouse_bytes(x10, 0, 10, report),
            [0x1b, b'[', b'M', b'@', 40, 35]
        );
        assert!(
            mouse_bytes(
                x10,
                0,
                10,
                MouseReport {
                    press: false,
                    ..report
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn the_alternate_screen_takes_the_viewport_and_gives_it_back() {
        let mut terminal = Terminal::new((800.0, 160.0), (8.0, 16.0));
        terminal.feed(b"the session\r\n");
        terminal.feed(b"\x1b[?1049hfull screen\x1b[?1049l");
        let screen = terminal.screen_text();
        assert!(
            screen.contains("the session"),
            "the session came back: {screen}"
        );
        assert!(
            !screen.contains("full screen"),
            "the program stayed: {screen}"
        );
    }
}
