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
/// How quiet the output pipe of an exited child has to be before its last
/// frame is taken as complete, and how long to wait for that in total.
const EXIT_TAIL: Duration = Duration::from_millis(20);
const EXIT_LIMIT: Duration = Duration::from_millis(200);
/// A child that is given a screen of its own gets at least this many rows,
/// which is what `cmd` needs to print its banner and a prompt.
const MIN_SCREEN: usize = 4;

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

pub struct Terminal {
    term: Term<Proxy>,
    parser: Processor,
    size: Size,
    /// The size the grid is currently at: the whole tab while kite types, the
    /// rows below the prompt while a child writes text, and the whole tab again
    /// while a child holds the alternate screen.
    screen: Size,
    /// Where the child's screen begins: the row the prompt left behind.
    job_origin: usize,
    cell: (f32, f32),
    job: Option<Job>,
    exit: Option<Option<ExitStatus>>,
    events: Receiver<TermEvent>,
    /// Where the drag started and where it stands, in grid cells.
    selection: Option<(Point, Point)>,
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
            screen: size,
            job_origin: 0,
            size,
            cell,
            job: None,
            exit: None,
            events,
            selection: None,
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

    /// The whole tab joined, trailing blanks trimmed.
    #[cfg(test)]
    pub fn screen_text(&self) -> String {
        (0..self.size.screen_lines as i32)
            .map(|row| {
                self.row_text(self.first_line() + row)
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The grid line sitting under the top of the screen. Zero means the view
    /// is at the bottom; negative means it is scrolled back into history.
    pub fn top_line(&self) -> i32 {
        -(self.term.grid().display_offset() as i32)
    }

    /// Tab rows that belong to the session rather than to the screen the grid
    /// is showing: the history the child's own screen was placed under.
    pub fn origin(&self) -> usize {
        self.size.screen_lines - self.screen.screen_lines
    }

    /// The grid line at the top row of the tab, history included. Every row the
    /// tab shows is this line plus its distance from the top.
    pub fn first_line(&self) -> i32 {
        self.top_line() - self.origin() as i32
    }

    /// Whether the view is looking at history rather than the live bottom.
    pub fn scrolled(&self) -> bool {
        self.term.grid().display_offset() != 0
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// The first grid line there is. The tab's top row cannot ask for anything
    /// above it without drawing a blank.
    pub fn topmost_line(&self) -> i32 {
        self.term.grid().topmost_line().0
    }

    /// Rows of scrollback above the viewport.
    pub fn history(&self) -> i32 {
        self.term.grid().history_size() as i32
    }

    /// Move the view. Positive `lines` goes down towards the present.
    ///
    /// The tab draws `origin` rows above the grid's own viewport, so the history
    /// runs out that many lines before alacritty's own clamp says it does. Going
    /// past there asks the grid for lines it never had, and every one of them
    /// comes back blank: the top of the tab empties out while the view is
    /// already sitting on the first line there is.
    pub fn scroll_by(&mut self, lines: i32) {
        if lines == 0 {
            return;
        }
        let limit = (-self.topmost_line() - self.origin() as i32).max(0);
        let wanted = (self.display_offset() as i32 - lines).clamp(0, limit);
        let delta = wanted - self.display_offset() as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
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
        // A drag that starts on a row above the top of the grid has no line to
        // name, so it begins at the first one there is.
        let top = self.term.grid().topmost_line();
        let clamp = |point: Point| Point::new(point.line.max(top), point.column);
        self.selection = Some((clamp(anchor), clamp(cursor)));
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
        let bytes = mouse_bytes(mode, self.top_line(), self.screen.screen_lines, report);
        if !bytes.is_empty() {
            self.send(&bytes);
        }
    }

    /// One visible row, cell by cell. A wide glyph is one entry and its
    /// trailing spacer is dropped, so the count can be under `columns`.
    ///
    /// A row the grid has no line for comes back empty. The tab's rows above a
    /// child's screen are the session's history, and a program that took the
    /// alternate screen leaves that history behind empty, so those rows have
    /// nothing of their own to show until it gives the screen back.
    pub fn cells(&self, line: i32) -> Vec<(char, Option<Color>, bool)> {
        let grid = self.term.grid();
        let row = Line(line);
        if row < grid.topmost_line() {
            return Vec::new();
        }
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

    /// Whether one cell holds a glyph at all. A blank, and the tail half of a
    /// wide character, hold none.
    pub fn glyph_at(&self, line: i32, column: usize) -> bool {
        let grid = self.term.grid();
        let row = Line(line);
        if row < grid.topmost_line() || column >= self.size.columns {
            return false;
        }
        let cell = &grid[Point::new(row, Column(column))];
        cell.c != ' ' && !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
    }

    /// The colour the grid gave one cell, and nothing for a blank or default
    /// one. The typing powder borrows the colour of the cell left of the caret.
    pub fn cell_color(&self, line: i32, column: usize) -> Option<Color> {
        if !self.glyph_at(line, column) {
            return None;
        }
        let grid = self.term.grid();
        resolve(grid[Point::new(Line(line), Column(column))].fg)
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

    /// Which column of the grid the caret stands on, whether or not the child
    /// is showing one.
    pub fn cursor_column(&self) -> usize {
        self.term.renderable_content().cursor.point.column.0
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

    /// Which row of the tab the cursor is standing on. Scrolling moves the
    /// view, not the caret, so this ignores where the view happens to be.
    pub fn cursor_row(&self) -> usize {
        (self.cursor_line() + self.origin() as i32).max(0) as usize
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

    /// The size the grid should be: the rows below the prompt while a child
    /// holds its own screen, and the whole tab otherwise. A program that
    /// believes it has been handed a fresh terminal — `cmd` prints its banner
    /// three rows down — is only ever right about its own screen, so that is
    /// what it gets.
    ///
    /// The alternate screen is no exception, because giving a program the whole
    /// tab while it holds the alternate screen makes its hand-back repaint
    /// erase the session above: the program's own main buffer is empty (each
    /// command starts a fresh pseudo console), so the frame it paints on the
    /// way out overwrites the prompt and history that share the grid with it.
    /// Keeping the program inside the rows below the prompt means its repaints
    /// can only reach its own rows.
    fn want(&self) -> Size {
        let rows = if self.job_origin > 0 {
            self.size.screen_lines - self.job_origin
        } else {
            self.size.screen_lines
        };
        Size::new(self.size.columns, rows)
    }

    /// Put the grid at the size its owner needs and tell the child about the
    /// screen it just got. Rows the grid loses go to history, and come back
    /// when the child is done with them.
    fn fit(&mut self) {
        let want = self.want();
        if want == self.screen {
            return;
        }
        self.screen = want;
        self.term.resize(want.with_history());
        if let Some(job) = self.job.as_ref() {
            let _ = job.writer.send(Msg::Resize(self.window_size()));
        }
    }

    /// Give a child the rows below the cursor as its own screen.
    ///
    /// The screen has to be the child's alone, which means the rows it takes
    /// over have to be empty and the session's rows have to be above it. Only
    /// scrolling does that — a shrink that has to scroll keeps what it loses,
    /// and one that does not simply drops it — so the cursor is walked to the
    /// bottom of the tab and the screen is scrolled into being, one row at a
    /// time, before the grid is taken down to its size.
    fn give_screen_below_cursor(&mut self) {
        let rows = self.size.screen_lines;
        let row = (self.cursor_line() - self.first_line()).max(0) as usize;
        let least = (rows / 2).max(MIN_SCREEN);
        let screen = (rows - row).max(least).min(rows);
        self.job_origin = rows - screen;
        if screen > 1 {
            self.feed(&b"\r\n".repeat(screen - 1));
        }
        self.fit();
        // The padding left the cursor at the bottom of the rows just carved
        // out. A program that waits for input without painting first (a bare
        // `scanf`) would then show its caret there, far below its command. Home
        // it to the top of the child's screen so the caret sits on the line
        // directly under the command, the way any terminal leaves it.
        self.feed(b"\x1b[H");
    }

    /// Attach a pseudo console to `program`, so it gets a real screen: the rows
    /// of the tab below whatever the prompt left behind.
    pub fn start(&mut self, program: &Path, args: &[String], cwd: &Path) -> io::Result<()> {
        self.close_job();
        self.give_screen_below_cursor();
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
    /// that wrapped at the old width re-wraps at the new one, and a child that
    /// is on a screen of its own gets the rows the taller tab left behind.
    pub fn resize(&mut self, panel: (f32, f32), cell: (f32, f32)) {
        let size = cells(panel, cell);
        if size == self.size && cell == self.cell {
            return;
        }
        self.cell = cell;
        self.size = size;
        let least = (size.screen_lines / 2).max(MIN_SCREEN);
        self.job_origin = self.job_origin.min(size.screen_lines.saturating_sub(least));
        self.fit();
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
                    self.feed(&bytes);
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

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.screen.screen_lines as u16,
            num_cols: self.screen.columns as u16,
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
        // A child that dies inside the alternate screen leaves its own screen
        // standing where the session's rows are. Once it is gone the rows belong
        // to the session again.
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.parser.advance(&mut self.term, b"\x1b[?1049l");
        }
        self.job_origin = 0;
        self.fit();
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
            // The console host batches, so the last frame of a program that
            // writes and quits tends to arrive a few milliseconds after the
            // process is already gone. Chase it: read until the pipe has been
            // quiet for a moment, with a limit for a host that never settles.
            // `read` answers `Ok(0)` for both empty and closed, so the pipe
            // itself cannot say when it is finished.
            let limit = Instant::now() + EXIT_LIMIT;
            let mut quiet = Instant::now();
            loop {
                let mut arrived = false;
                while let Ok(read) = pty.reader().read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    arrived = true;
                    if news.send(News::Bytes(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
                if arrived {
                    quiet = Instant::now();
                } else if quiet.elapsed() >= EXIT_TAIL || Instant::now() >= limit {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
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

    /// Which tab row a line of text is sitting on, or -1 when it is on none.
    fn row_of(terminal: &Terminal, text: &str) -> i32 {
        (0..terminal.size.screen_lines as i32)
            .find(|row| terminal.row_text(terminal.first_line() + row).trim_end() == text)
            .unwrap_or(-1)
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

    /// The interrupt is a byte, and the child's own line discipline answers
    /// for it: a program waiting at a keyboard takes `^C` as its cue, which is
    /// all a pseudo console lets a terminal do from the outside.
    #[test]
    fn an_interrupt_ends_a_child_that_is_waiting_for_input() {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        let args = vec!["/c".to_string(), "pause".to_string()];
        terminal
            .start(
                Path::new("cmd.exe"),
                &args,
                &std::env::current_dir().unwrap(),
            )
            .expect("the pseudo console should start");
        let mut waiting = false;
        for _ in 0..160 {
            terminal.poll();
            if terminal.screen_text().contains("pause") || terminal.screen_text().contains('键') {
                waiting = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(waiting, "the child never reached its prompt");
        terminal.send(&[0x03]);
        for _ in 0..160 {
            terminal.poll();
            if !terminal.running() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the child ignored the interrupt");
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
            "the screen the child was given took the session with it: {screen:?}"
        );
    }

    /// A program that writes and leaves has written all of it: the exit is not
    /// reported until its output stops arriving, so the tail of a build log is
    /// not lost to the race between the process and its console host.
    #[test]
    fn a_child_that_leaves_quickly_has_still_finished_writing() {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        let args = vec![
            "/c".to_string(),
            "for /L %i in (1,1,200) do @echo line %i".to_string(),
        ];
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
        assert!(
            screen.contains("line 199") && screen.contains("line 200"),
            "the last lines never arrived:\n{screen}"
        );
    }

    /// A child is given the rows below the prompt, which is the only screen
    /// whose first row it can be right about: `cmd` prints its banner three
    /// rows down because it believes the terminal it was handed is fresh.
    #[test]
    fn a_child_is_given_the_rows_below_the_prompt() {
        let mut terminal = screen(b"the session so far\r\n");
        terminal.give_screen_below_cursor();
        assert_eq!(terminal.origin(), 1);
        assert_eq!(terminal.screen.screen_lines, 29);
        // What ConPTY opens with: the buffer it just made, cleared and homed.
        terminal.feed(b"\x1b[2J\x1b[HMicrosoft Windows [Version]\r\nX:\\>");
        let shown = terminal.screen_text();
        assert!(shown.contains("X:\\"), "{shown}");
        assert_eq!(
            terminal.row_text(terminal.first_line()).trim_end(),
            "the session so far",
            "the session keeps the row above the child's screen"
        );
        assert_eq!(
            terminal.row_text(terminal.first_line() + 1).trim_end(),
            "Microsoft Windows [Version]",
            "the child's first row is the one below it"
        );
    }

    /// The full round trip with a program that takes the screen and gives it
    /// back, byte for byte the way ConPTY sends it: the session it covered is
    /// still there, in order, above the rows the child was given.
    #[test]
    fn a_full_screen_program_leaves_the_session_where_it_was() {
        let mut terminal = Terminal::new((800.0, 496.0), (8.0, 16.0));
        for row in 1..=28 {
            terminal.feed(format!("row {row}\r\n").as_bytes());
        }
        terminal.feed(b"the prompt\r\n");
        assert_eq!(terminal.cursor_line(), 29, "the tab is nearly full");
        terminal.give_screen_below_cursor();
        assert_eq!(terminal.screen.screen_lines, 15);
        assert_eq!(terminal.origin(), 16);
        assert_eq!(
            row_of(&terminal, "the prompt"),
            15,
            "the prompt keeps the row above the child's screen"
        );

        terminal.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen painting");
        assert_eq!(
            terminal.origin(),
            16,
            "the alternate screen is the same strip"
        );
        assert_eq!(
            row_of(&terminal, "full screen painting"),
            16,
            "the child paints the rows it was given"
        );
        assert_eq!(
            row_of(&terminal, "the prompt"),
            -1,
            "the session is on the other buffer while the child paints"
        );

        // What ConPTY really sends on the way out: the swap inside a chunk, a
        // window report, then the buffer it swapped to walked row by row.
        terminal.feed(b"\r\x1b[K\x1b[?25l\x1b[140C\x1b[?1049l");
        terminal.feed(b"\x1b[23;0;0t");
        let mut repaint = b"\x1b[H".to_vec();
        for _ in 0..terminal.screen.screen_lines {
            repaint.extend(b"\x1b[K\r\n");
        }
        repaint.extend(b"\x1b[H\x1b[?25h");
        terminal.feed(&repaint);
        assert_eq!(terminal.origin(), 16, "the strip is the child's still");

        terminal.close_job();
        let shown = terminal.screen_text();
        for row in 15..=28 {
            assert!(
                shown.contains(&format!("row {row}")),
                "the session lost row {row}:\n{shown}"
            );
        }
        assert!(shown.contains("the prompt"), "no prompt:\n{shown}");
    }

    /// A session that already fills the tab gives its child a screen of at least
    /// half the tab and finds the rows by scrolling, so the prompt ends up on
    /// the row above the child's screen rather than underneath it.
    #[test]
    fn a_full_tab_rolls_up_to_make_room_for_a_child() {
        let mut terminal = Terminal::new((800.0, 480.0), (8.0, 16.0));
        for row in 1..=29 {
            terminal.feed(format!("row {row}\r\n").as_bytes());
        }
        terminal.feed(b"the prompt\r\n");
        assert_eq!(terminal.cursor_line(), 29, "the tab is full to the bottom");
        terminal.give_screen_below_cursor();
        assert_eq!(terminal.screen.screen_lines, 15);
        assert_eq!(terminal.origin(), 15);
        terminal.feed(b"\x1b[2J\x1b[Hchild speaks");
        let shown = terminal.screen_text();
        let lines: Vec<&str> = shown.lines().map(str::trim_end).collect();
        assert_eq!(
            lines.iter().position(|line| *line == "the prompt"),
            Some(14),
            "the prompt keeps the row above the child's screen:\n{shown}"
        );
        assert_eq!(
            lines.iter().position(|line| *line == "child speaks"),
            Some(15),
            "the child's first row is the one below it:\n{shown}"
        );
    }

    /// A program that asks for the alternate screen is given the same rows as
    /// any other child. Clearing and repainting them on the way in and on the
    /// way out is exactly what it may do to a screen of its own, and the
    /// session above them is on rows it was never given.
    #[test]
    fn a_full_screen_program_is_given_the_rows_below_the_prompt_too() {
        let mut terminal = screen(b"the session so far\r\n");
        terminal.give_screen_below_cursor();
        terminal.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        assert_eq!(
            terminal.origin(),
            1,
            "the alternate screen is the same strip"
        );
        assert_eq!(row_of(&terminal, "full screen"), 1);

        // What ConPTY really sends on the way back: the swap, a title pop, then
        // the buffer it swapped to painted row by row.
        terminal.feed(b"\r\x1b[K\x1b[?25l\x1b[140C\x1b[?1049l");
        terminal.feed(b"\x1b[23;0;0t");
        terminal.feed(b"\x1b[H\x1b[K\r\n\x1b[K\r\n\x1b[K\x1b[H\x1b[?25h");
        assert_eq!(terminal.origin(), 1, "the strip is the child's still");
        let shown = terminal.screen_text();
        assert!(
            shown.contains("the session so far"),
            "the repaint of the child's buffer wiped the session:\n{shown}"
        );
    }

    #[test]
    fn a_child_that_writes_after_giving_the_screen_back_is_heard() {
        let mut terminal = screen(b"the session so far\r\n");
        terminal.give_screen_below_cursor();
        terminal.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        terminal.feed(b"\x1b[?1049l\x1b[H\x1b[K\r\n");
        terminal.feed(b"the choice\r\n");
        let shown = terminal.screen_text();
        assert!(
            shown.contains("the choice") && shown.contains("the session so far"),
            "{shown}"
        );
    }

    #[test]
    fn a_child_that_dies_in_the_alternate_screen_leaves_it_with_the_session() {
        let mut terminal = screen(b"the session so far\r\n");
        terminal.give_screen_below_cursor();
        terminal.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
        terminal.close_job();
        assert_eq!(terminal.origin(), 0, "the tab is the session's again");
        assert!(
            terminal.screen_text().contains("the session so far"),
            "the session was still behind the screen the child never left"
        );
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
    /// The tab is taller than the grid it reads: the rows above a child's
    /// screen belong to the session. So the view has to stop `origin` lines
    /// before alacritty's own clamp, or the top of the tab starts asking the
    /// grid for lines that never existed and draws blanks over real content.
    #[test]
    fn scrolling_up_stops_where_the_history_runs_out() {
        let mut terminal = Terminal::new((800.0, 496.0), (8.0, 16.0));
        for row in 1..=28 {
            terminal.feed(format!("row {row}\r\n").as_bytes());
        }
        terminal.feed(b"the prompt\r\n");
        terminal.give_screen_below_cursor();
        assert_eq!(
            terminal.origin(),
            16,
            "the tab shows 16 rows above the child"
        );
        terminal.feed(b"child painting\r\n");

        let top = terminal.topmost_line();
        assert!(top < 0, "the rows the band pushed out are history");

        for _ in 0..40 {
            terminal.scroll_by(-3);
        }
        assert_eq!(
            terminal.first_line(),
            top,
            "the tab's top row sits on the first line the grid has"
        );
        assert!(
            !terminal.row_text(terminal.first_line()).trim().is_empty(),
            "and that row is content, not a blank: {:?}",
            terminal.row_text(terminal.first_line())
        );

        // Coming back down has to reach the live bottom, clamp or no clamp.
        for _ in 0..40 {
            terminal.scroll_by(3);
        }
        assert_eq!(terminal.display_offset(), 0);
        assert!(!terminal.scrolled());
    }
}
