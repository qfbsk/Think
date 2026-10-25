//! The miku board: one's own saving history, laid out like GitHub's heat map.
//!
//! One calendar year to a screen, every day of it drawn whether or not it has
//! happened yet. Columns are weeks and rows are Sunday to Saturday; the squares
//! stretch with the panel, and the year on either side is always one page away.

use glyphon::FontSystem;
use glyphon::cosmic_text::{Buffer, Color, Metrics, Wrap};

use crate::activity;
use crate::day::Day;
use crate::text::{bold_attrs, make_label, measure};
use crate::theme;
use crate::ui::Rect;

/// The year takes the pitch the panel allows between these two: wide enough to
/// fill a normal window, small enough to fit a squeezed one, capped so an
/// ultrawide does not turn the grid into a wall of blocks.
const MAX_STEP: f32 = 32.0;
const MIN_STEP: f32 = 4.0;
/// The square is three quarters of the pitch, the way GitHub spaces them.
const CELL_RATIO: f32 = 0.75;
/// The board's type is a proportion of the pitch, not of the body text size.
const TEXT_RATIO: f32 = 0.47;
const MIN_TEXT: f32 = 7.0;
const MAX_TEXT: f32 = 15.0;
const GUTTER: f32 = 34.0;
const HEAD: f32 = 22.0;
const ROWS: i64 = 7;
const LEVELS: usize = 6;
/// The years the board is willing to draw. Paging is not capped by the history,
/// so these are numbers only arithmetic could ever reach.
const FIRST_YEAR: i64 = 1900;
const LAST_YEAR: i64 = 2999;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// One square of the grid, with everything the painter and the mouse need.
struct Cell {
    x: f32,
    y: f32,
    day: Day,
    level: usize,
}

pub struct Miku {
    probe: activity::Probe,
    /// The calendar year on screen; paging moves it, the log never bounds it.
    year: i64,
    /// Weeks the displayed year spans, and the pitch the panel allows for them.
    columns: usize,
    week_start: Day,
    step: f32,
    cell: f32,
    selected: Day,
    origin: (f32, f32),
    cells: Vec<Cell>,
    /// The five legend squares, which are paint but never a hit target.
    legend: Vec<(f32, f32, usize)>,
    labels: Vec<(Buffer, f32, f32)>,
    body: Buffer,
    family: &'static str,
    scale: f32,
    width: f32,
    height: f32,
    dirty: bool,
}

impl Miku {
    pub fn new(font_system: &mut FontSystem, family: &'static str, scale: f32) -> Self {
        Self::build(font_system, family, scale, activity::log_path())
    }

    fn build(
        font_system: &mut FontSystem,
        family: &'static str,
        scale: f32,
        log: Option<std::path::PathBuf>,
    ) -> Self {
        let today = Day::today();
        let step = MAX_STEP * scale;
        let text = step * TEXT_RATIO;
        let metrics = Metrics::new(text, (text * 1.4).ceil());
        let mut body = Buffer::new_empty(metrics);
        body.set_wrap(Wrap::None);
        let mut page = Self {
            probe: activity::Probe::new(log),
            year: today.date().0,
            columns: 53,
            week_start: today.week_start(),
            step,
            cell: step * CELL_RATIO,
            selected: today,
            origin: (0.0, 0.0),
            cells: Vec::new(),
            legend: Vec::new(),
            labels: Vec::new(),
            body,
            family,
            scale,
            width: 1.0,
            height: 1.0,
            dirty: true,
        };
        page.shape(font_system);
        page
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        let (width, height) = (width.max(1.0), height.max(1.0));
        if (width - self.width).abs() < 0.01 && (height - self.height).abs() < 0.01 {
            return;
        }
        self.width = width;
        self.height = height;
        self.dirty = true;
        self.shape(font_system);
    }

    /// The Sunday the displayed year starts on, and how many weeks it spans.
    fn year_grid(&self, year: i64) -> (Day, usize) {
        let first = Day::from_date(year, 1, 1).week_start();
        let last = Day::from_date(year, 12, 31);
        (first, (last.0 - first.0) as usize / 7 + 1)
    }

    /// One year to a screen: the squares take whatever pitch the panel allows,
    /// so the year is always whole and never wider than the page.
    fn fit(&mut self) {
        let (first, columns) = self.year_grid(self.year);
        self.columns = columns;
        self.week_start = first;
        let room = (self.width - (GUTTER + 28.0) * self.scale).max(MIN_STEP * self.scale);
        self.step = (room / columns as f32).clamp(MIN_STEP * self.scale, MAX_STEP * self.scale);
        self.cell = self.step * CELL_RATIO;
    }

    /// The board's type belongs to the picture, not to the text pages: it is a
    /// share of the week pitch, so it follows the width of the panel and ignores
    /// the body text size. The bounds keep it readable at both extremes.
    fn text_size(&self) -> f32 {
        (self.step * TEXT_RATIO).clamp(MIN_TEXT * self.scale, MAX_TEXT * self.scale)
    }

    pub fn retheme(&mut self, font_system: &mut FontSystem) {
        self.dirty = true;
        self.shape(font_system);
    }

    pub fn poll(&mut self, font_system: &mut FontSystem) -> bool {
        if !self.probe.poll() {
            return false;
        }
        self.dirty = true;
        self.shape(font_system);
        true
    }

    pub fn pending(&self) -> bool {
        self.probe.pending()
    }

    pub fn record_save(&mut self, font_system: &mut FontSystem, path: &str) {
        self.probe.record(Day::today(), path);
        self.dirty = true;
        self.shape(font_system);
    }

    fn week_of(&self, column: usize) -> Day {
        self.week_start.plus(column as i64 * 7)
    }

    /// Keep the screen on the year the cursor is in.
    fn follow(&mut self) {
        self.year = self.selected.date().0.clamp(FIRST_YEAR, LAST_YEAR);
    }

    fn reveal(&mut self) {
        // A year the reader has not walked to yet asks for another block.
        let oldest = Day::from_date(self.year, 1, 1);
        if !self.probe.exhausted() && self.probe.edge().map_or(true, |edge| oldest <= edge) {
            self.probe.want_more();
        }
        self.dirty = true;
    }

    pub fn nudge(&mut self, font_system: &mut FontSystem, days: i64) {
        self.selected = self.selected.plus(days);
        self.follow();
        self.reveal();
        self.shape(font_system);
    }

    /// Paging turns a whole year over, further back for a positive count. Neither
    /// direction stops anywhere: tomorrow is as scrollable as last century.
    pub fn page(&mut self, font_system: &mut FontSystem, years: i64) {
        self.year = (self.year - years).clamp(FIRST_YEAR, LAST_YEAR);
        let (_, month, day) = self.selected.date();
        self.selected = if Day::from_date(self.year, month, day).date() == (self.year, month, day) {
            Day::from_date(self.year, month, day)
        } else {
            // 29 February only comes round some years.
            Day::from_date(self.year, month, day - 1)
        };
        self.reveal();
        self.shape(font_system);
    }

    pub fn select(&mut self, font_system: &mut FontSystem, day: Day) {
        self.selected = day;
        self.follow();
        self.reveal();
        self.shape(font_system);
    }

    /// The day under a click, if a square is really there.
    pub fn hit(&self, x: f32, y: f32) -> Option<Day> {
        self.cells
            .iter()
            .find(|cell| {
                x >= cell.x && x < cell.x + self.cell && y >= cell.y && y < cell.y + self.cell
            })
            .map(|cell| cell.day)
    }

    pub fn status(&self) -> String {
        format!(
            "{}  {} saves",
            self.selected.label(),
            self.probe.total(self.selected)
        )
    }

    pub fn quads(&self) -> Vec<(Rect, Color, f32)> {
        let size = self.cell;
        // A little rounding, not a pill: enough to soften the grid at the
        // largest pitch and to vanish entirely at the smallest.
        let round = size * 0.16;
        let mut quads: Vec<(Rect, Color, f32)> = self
            .cells
            .iter()
            .map(|cell| {
                (
                    Rect {
                        x: cell.x,
                        y: cell.y,
                        w: size,
                        h: size,
                    },
                    theme::miku(cell.level),
                    round,
                )
            })
            .chain(self.legend.iter().map(|(x, y, level)| {
                (
                    Rect {
                        x: *x,
                        y: *y,
                        w: size,
                        h: size,
                    },
                    theme::miku(*level),
                    round,
                )
            }))
            .collect();
        if let Some(selected) = self
            .cells
            .iter()
            .find(|cell| cell.day == self.selected)
            .map(|cell| (cell.x, cell.y))
        {
            // A frame rather than a fill, so the level underneath stays readable.
            let edge = 1.0 * self.scale;
            let pad = 2.0 * self.scale;
            let outer = selected.0 - pad;
            let total = size + pad * 2.0;
            for (x, y, w, h) in [
                (outer, selected.1 - pad, total, edge),
                (outer, selected.1 + size + pad, total, edge),
                (outer, selected.1 - pad, edge, total),
                (selected.0 + size + pad, selected.1 - pad, edge, total),
            ] {
                quads.push((Rect { x, y, w, h }, theme::text(), 0.0));
            }
        }
        quads
    }

    pub fn areas(&self) -> Vec<(&Buffer, f32, f32)> {
        let mut areas: Vec<(&Buffer, f32, f32)> = self
            .labels
            .iter()
            .map(|(label, x, y)| (label, *x, *y))
            .collect();
        areas.push((
            &self.body,
            self.origin.0,
            self.grid_bottom() + 18.0 * self.scale,
        ));
        areas
    }

    fn grid_bottom(&self) -> f32 {
        self.origin.1 + (ROWS as f32 - 1.0) * self.step + self.cell
    }

    pub fn shape(&mut self, font_system: &mut FontSystem) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.fit();
        let pad = 14.0 * self.scale;
        self.origin = (GUTTER * self.scale + pad, HEAD * self.scale + pad);
        let step = self.step;
        let start = Day::from_date(self.year, 1, 1);
        let end = Day::from_date(self.year, 12, 31);
        let text = self.text_size();

        let mut cells = Vec::new();
        self.legend.clear();
        let mut labels: Vec<(Buffer, f32, f32)> = Vec::new();
        let mut previous_month = -1;
        let mut last_label = f32::NEG_INFINITY;
        for column in 0..self.columns {
            let week = self.week_of(column);
            let x = self.origin.0 + column as f32 * step;
            // The first and last week of a year lean on the years either side;
            // only this year's own days are counted, and drawn.
            let first = week.max(start);
            let (year, month, _) = first.date();
            if month != previous_month {
                previous_month = month;
                // Two months can start in neighbouring columns; the name only goes
                // where there is room for it.
                if x - last_label >= 3.0 * step {
                    last_label = x;
                    let name = if month == 1 {
                        format!("{} {}", MONTHS[0], year)
                    } else {
                        MONTHS[(month - 1) as usize].to_string()
                    };
                    let label = make_label(
                        font_system,
                        &bold_attrs(self.family, theme::text_dim()),
                        text * 0.85,
                        &name,
                    );
                    labels.push((label, x, self.origin.1 - 17.0 * self.scale));
                }
            }
            let last = week.plus(ROWS - 1).min(end);
            let mut day = first;
            while day <= last {
                let level = self.probe.level(day);
                cells.push(Cell {
                    x,
                    y: self.origin.1 + (day.0 - week.0) as f32 * step,
                    day,
                    level,
                });
                day = day.plus(1);
            }
        }

        for (row, name) in [(1, "Mon"), (3, "Wed"), (5, "Fri")] {
            let label = make_label(
                font_system,
                &bold_attrs(self.family, theme::text_dim()),
                text * 0.85,
                name,
            );
            let (_, height) = measure(&label);
            labels.push((
                label,
                4.0 * self.scale,
                self.origin.1 + row as f32 * step + (step - height) / 2.0,
            ));
        }

        // The legend sits under the grid, on its right, like the original:
        // `Less ▢▢▢▢▢ More`, all of it ending where the grid ends.
        let legend_y = self.grid_bottom() + 8.0 * self.scale;
        let right = self.origin.0 + (self.columns as f32 - 1.0) * step + self.cell;
        let more = make_label(
            font_system,
            &bold_attrs(self.family, theme::text_dim()),
            text * 0.85,
            "More",
        );
        let (more_w, _) = measure(&more);
        let squares_right = right - more_w - 8.0 * self.scale;
        let less_x = squares_right - LEVELS as f32 * step - 8.0 * self.scale;
        let less = make_label(
            font_system,
            &bold_attrs(self.family, theme::text_dim()),
            text * 0.85,
            "Less",
        );
        let (less_w, _) = measure(&less);
        labels.push((less, (less_x - less_w).max(self.origin.0), legend_y));
        labels.push((more, right - more_w, legend_y));
        for level in 0..LEVELS {
            self.legend.push((
                squares_right - (LEVELS - level) as f32 * step,
                legend_y,
                level,
            ));
        }

        let lines = self.detail();
        self.body = Buffer::new_empty(Metrics::new(text, (text * 1.4).ceil()));
        self.body.set_wrap(Wrap::None);
        self.body.set_size(None, None);
        let style = bold_attrs(self.family, theme::text());
        for line in lines {
            self.body
                .lines
                .push(crate::text::buffer_line(&line, &style));
        }
        if self.body.lines.is_empty() {
            self.body.lines.push(crate::text::buffer_line(" ", &style));
        }
        self.body.shape_until_scroll(font_system, false);
        self.cells = cells;
        self.labels = labels;
    }

    /// What the selected day was made of, one file per line, busiest first.
    fn detail(&self) -> Vec<String> {
        let files = self.probe.files(self.selected);
        if files.is_empty() {
            return vec![format!("nothing saved on {}", self.selected.label())];
        }
        let room = ((self.height - self.grid_bottom() - 40.0 * self.scale)
            / (self.text_size() * 1.4))
            .max(1.0) as usize;
        let mut lines: Vec<String> = files
            .iter()
            .take(room)
            .map(|(path, count)| {
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                let parent = std::path::Path::new(path)
                    .parent()
                    .map(|parent| parent.to_string_lossy().into_owned())
                    .unwrap_or_default();
                format!("{name}  \u{d7}{count}   {parent}")
            })
            .collect();
        if files.len() > lines.len() {
            lines.push(format!("... {} more", files.len() - lines.len()));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::fonts::Fonts;

    fn board(log: std::path::PathBuf, width: f32) -> (Fonts, Miku) {
        let mut fonts = Fonts::load();
        let mut page = Miku::build(&mut fonts.system, fonts.mono, 1.0, Some(log));
        page.set_viewport(&mut fonts.system, width, 600.0);
        for _ in 0..100 {
            if page.poll(&mut fonts.system) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (fonts, page)
    }

    #[test]
    fn one_screen_holds_exactly_one_calendar_year() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("activity.log");
        let today = Day::today();
        std::fs::write(&log, format!("{}\tE:\\a.rs\n", today.label()).into_bytes()).unwrap();

        let (_fonts, page) = board(log, 900.0);
        let days: Vec<Day> = page.cells.iter().map(|cell| cell.day).collect();
        let january = Day::from_date(page.year, 1, 1);
        assert_eq!(page.year, today.date().0, "the board opens on this year");
        let december = Day::from_date(page.year, 12, 31);
        assert!(days.contains(&january), "the year starts on the board");
        assert!(days.contains(&today), "and so does today");
        assert!(
            days.contains(&december),
            "and the days that have not happened yet"
        );
        assert!(
            days.iter().all(|day| *day >= january && *day <= december),
            "one screen holds exactly one calendar year"
        );
        assert_eq!(
            days.len(),
            (december.0 - january.0 + 1) as usize,
            "every day of the year gets a square"
        );
    }

    #[test]
    fn the_year_always_fits_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("activity.log");
        std::fs::write(&log, b"").unwrap();
        let (wide_fonts, wide) = board(log.clone(), 1200.0);
        let (narrow_fonts, narrow) = board(log, 420.0);
        drop((wide_fonts, narrow_fonts));
        for page in [&wide, &narrow] {
            let right = page.origin.0 + (page.columns as f32 - 1.0) * page.step + page.cell;
            assert!(
                right <= page.width,
                "{} weeks at {}px must fit {}",
                page.columns,
                page.step,
                page.width
            );
        }
        assert!(narrow.step < wide.step, "the squares shrink with the panel");
        assert!(
            narrow.text_size() < wide.text_size(),
            "and so does the type, which the body text size does not touch"
        );
    }

    #[test]
    fn paging_turns_whole_years_and_never_stops() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("activity.log");
        let today = Day::today();
        std::fs::write(&log, format!("{}\tE:\\a.rs\n", today.label()).into_bytes()).unwrap();

        let (mut fonts, mut page) = board(log, 900.0);
        let (year, month, _) = today.date();
        page.page(&mut fonts.system, 1);
        assert_eq!(page.year, year - 1, "one page, one year");
        assert_eq!(page.selected.date().1, month, "on the same date");
        page.page(&mut fonts.system, 30);
        assert_eq!(page.year, year - 31, "the log running out does not stop it");
        let whole = Day::from_date(page.year, 12, 31).0 - Day::from_date(page.year, 1, 1).0 + 1;
        assert_eq!(
            page.cells.len(),
            whole as usize,
            "and a finished year is drawn whole"
        );
        page.page(&mut fonts.system, -1_000);
        assert!(
            page.year > year,
            "forward is not stopped by today either: {}",
            page.year
        );
        let ahead = Day::from_date(page.year, 12, 31).0 - Day::from_date(page.year, 1, 1).0 + 1;
        assert_eq!(
            page.cells.len(),
            ahead as usize,
            "a year not lived yet is drawn all the same"
        );
        assert_eq!(page.probe.level(page.selected), 0, "and it is empty");
    }

    #[test]
    fn the_ramp_and_the_detail_follow_the_selected_day() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("activity.log");
        let today = Day::today();
        let busy = today.plus(-3);
        let mut body = String::new();
        for _ in 0..12 {
            body.push_str(&format!("{}\tE:\\Think\\big.rs\n", busy.label()));
        }
        body.push_str(&format!("{}\tE:\\Think\\small.rs\n", today.label()));
        std::fs::write(&log, body).unwrap();

        let (mut fonts, mut page) = board(log, 900.0);
        page.select(&mut fonts.system, busy);
        assert_eq!(page.status(), format!("{}  12 saves", busy.label()));
        let detail = page.detail();
        assert!(detail[0].starts_with("big.rs"), "{detail:?}");
        assert!(detail[0].contains('×'), "counts are part of the line");
        let level = |page: &Miku, day: Day| -> usize {
            page.cells
                .iter()
                .find(|cell| cell.day == day)
                .map(|cell| cell.level)
                .unwrap()
        };
        assert!(
            level(&page, busy) > level(&page, today),
            "the busy day sits higher up the ramp"
        );
        assert_eq!(level(&page, busy.plus(-1)), 0, "an empty day is level 0");
    }
}
