//! The local activity log behind the miku board.
//!
//! One line per save, appended and never rewritten: `2026-10-22\tpath`. The
//! board reads it backwards in blocks, so an unlimited history costs one block
//! at a time and a save costs one line.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::day::Day;

/// How much of the log a single page-back reads.
const BLOCK: u64 = 256 * 1024;

/// `%USERPROFILE%\.config\kite\activity.log`, next to `theme.toml`.
pub fn log_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("kite")
            .join("activity.log"),
    )
}

/// One day: how many saves, and which files they touched.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    pub total: usize,
    /// Highest first, so the detail list reads as a ranking.
    pub files: Vec<(String, usize)>,
}

impl Tally {
    fn add(&mut self, path: &str) {
        self.total += 1;
        match self.files.iter_mut().find(|(name, _)| name == path) {
            Some((_, count)) => *count += 1,
            None => self.files.push((path.to_string(), 1)),
        }
        self.files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    }

    fn merge(&mut self, other: Tally) {
        for (path, count) in other.files {
            match self.files.iter_mut().find(|(name, _)| *name == path) {
                Some((_, mine)) => *mine += count,
                None => self.files.push((path, count)),
            }
        }
        self.total += other.total;
        self.files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    }
}

#[derive(Debug, Default)]
struct History {
    days: BTreeMap<Day, Tally>,
    /// The oldest day the reader has walked past; anything older is still in the file.
    oldest: Option<Day>,
    exhausted: bool,
}

impl History {
    fn absorb(&mut self, chunk: Chunk) {
        for (day, tally) in chunk.days {
            match self.days.get_mut(&day) {
                Some(mine) => mine.merge(tally),
                None => {
                    self.days.insert(day, tally);
                }
            }
            self.oldest = Some(self.oldest.map_or(day, |held: Day| day.min(held)));
        }
        self.exhausted |= chunk.exhausted;
    }

    fn record(&mut self, day: Day, path: &str) {
        self.days.entry(day).or_default().add(path);
        self.oldest = Some(self.oldest.map_or(day, |held: Day| held.min(day)));
    }

    /// The heat of a day, by fixed thresholds. Fifty saves hammering one file is
    /// a different kind of day from fifty spread over a dozen, so the ramp counts
    /// the files as well as the saves.
    fn level(&self, day: Day) -> usize {
        let Some(tally) = self.days.get(&day) else {
            return 0;
        };
        let files = tally.files.len();
        if tally.total >= 50 {
            if files > 10 {
                5
            } else if files >= 3 {
                4
            } else {
                3
            }
        } else if tally.total >= 10 {
            2
        } else if tally.total >= 1 {
            1
        } else {
            0
        }
    }
}

/// What one block of the log turned into.
struct Chunk {
    days: Vec<(Day, Tally)>,
    exhausted: bool,
}

fn parse(text: &str) -> Vec<(Day, Vec<String>)> {
    let mut found: Vec<(Day, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let Some((date, path)) = line.split_once('\t') else {
            continue;
        };
        let Some(day) = Day::parse(date.trim()) else {
            continue;
        };
        match found.last_mut() {
            Some((held, _)) if *held == day => found.last_mut().unwrap().1.push(path.to_string()),
            _ => found.push((day, vec![path.to_string()])),
        }
    }
    found
}

fn tally(days: Vec<(Day, Vec<String>)>) -> Vec<(Day, Tally)> {
    days.into_iter()
        .map(|(day, paths)| {
            let mut tally = Tally::default();
            for path in paths {
                tally.add(&path);
            }
            (day, tally)
        })
        .collect()
}

/// Read `file` backwards from `cursor`, one block per call.
///
/// `suffix` holds the tail of a line whose head is still further back, which is
/// how a block boundary in the middle of a path stays one path.
fn read_backwards(file: &mut File, cursor: &mut u64, suffix: &mut Vec<u8>) -> (Vec<u8>, bool) {
    let end = *cursor;
    if end == 0 {
        return (std::mem::take(suffix), true);
    }
    let start = end.saturating_sub(BLOCK);
    let mut block = vec![0u8; (end - start) as usize];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut block).is_err() {
        return (Vec::new(), true);
    }
    *cursor = start;
    block.extend(suffix.iter());
    suffix.clear();
    if start == 0 {
        return (block, true);
    }
    match block.iter().rposition(|byte| *byte == b'\n') {
        Some(newline) => {
            suffix.extend(block.drain(newline + 1..));
            block.truncate(newline + 1);
            (block, false)
        }
        // A line longer than a block: keep going back until its head shows up.
        None => {
            *suffix = block;
            (Vec::new(), false)
        }
    }
}

fn worker(mut file: File, mut cursor: u64, inbox: Receiver<()>, out: Sender<Chunk>) {
    let mut suffix = Vec::new();
    while inbox.recv().is_ok() {
        let (bytes, exhausted) = read_backwards(&mut file, &mut cursor, &mut suffix);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let days = tally(parse(&text));
        if out.send(Chunk { days, exhausted }).is_err() {
            break;
        }
        if exhausted {
            break;
        }
    }
}

pub struct Probe {
    history: History,
    requests: Sender<()>,
    replies: Receiver<Chunk>,
    path: Option<PathBuf>,
    in_flight: usize,
}

impl Probe {
    /// Open the log and ask for the newest block right away.
    pub fn new(path: Option<PathBuf>) -> Self {
        let opened = path.as_ref().and_then(|path| File::open(path).ok());
        let (requests, inbox) = mpsc::channel::<()>();
        let (out, replies) = mpsc::channel();
        let reading = opened.is_some();
        if let Some(file) = opened {
            // The writer appends while we read, so the walk works from a fixed end.
            let cursor = file.metadata().map(|meta| meta.len()).unwrap_or_default();
            thread::Builder::new()
                .name("activity-log".to_string())
                .spawn(move || worker(file, cursor, inbox, out))
                .expect("spawn activity-log worker");
        }
        let mut probe = Self {
            history: History {
                // No file is a history that has been read to the bottom: the
                // board still pages over it, it just never asks for a block.
                exhausted: !reading,
                ..History::default()
            },
            requests,
            replies,
            path,
            in_flight: 0,
        };
        if reading {
            probe.ask();
        }
        probe
    }

    pub fn pending(&self) -> bool {
        self.in_flight > 0
    }

    fn ask(&mut self) {
        if self.path.is_some() && self.requests.send(()).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Page further back, once the board needs days we have not read.
    pub fn want_more(&mut self) {
        if self.in_flight == 0 && !self.history.exhausted {
            self.ask();
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for chunk in self.replies.try_iter() {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.history.absorb(chunk);
            changed = true;
        }
        changed
    }

    /// Append one save and fold it straight into the day it belongs to.
    pub fn record(&mut self, day: Day, path: &str) {
        if let Some(file) = self.path.as_ref().and_then(|path| open_append(path)) {
            append(file, day, path);
        }
        self.history.record(day, path);
    }

    pub fn total(&self, day: Day) -> usize {
        self.history.days.get(&day).map_or(0, |tally| tally.total)
    }

    pub fn files(&self, day: Day) -> &[(String, usize)] {
        self.history
            .days
            .get(&day)
            .map_or(&[] as &[(String, usize)], |tally| &tally.files)
    }

    pub fn level(&self, day: Day) -> usize {
        self.history.level(day)
    }

    /// How far the reader has walked: the oldest day it has seen in the file.
    pub fn edge(&self) -> Option<Day> {
        self.history.oldest
    }

    pub fn exhausted(&self) -> bool {
        self.history.exhausted
    }
}

fn open_append(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn append(mut file: File, day: Day, path: &str) {
    let line = format!("{}\t{}\n", day.label(), path);
    let _ = file.write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_boundary_in_the_middle_of_a_line_stays_one_line() {
        let lines: Vec<String> = (0..12_000)
            .map(|index| format!("2026-01-0{}\tE:\\Think\\file_{index}.rs", index % 9 + 1))
            .collect();
        let body = lines.join("\n") + "\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.log");
        std::fs::write(&path, body.as_bytes()).unwrap();

        let mut file = File::open(&path).unwrap();
        let length = file.metadata().unwrap().len();
        assert!(length > BLOCK, "the fixture must actually span blocks");
        let mut cursor = length;
        let mut suffix = Vec::new();
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        loop {
            let (bytes, exhausted) = read_backwards(&mut file, &mut cursor, &mut suffix);
            blocks.push(bytes);
            if exhausted {
                break;
            }
        }
        // Walking backwards hands the blocks back newest-first.
        blocks.reverse();
        let whole: Vec<u8> = blocks.concat();
        assert_eq!(
            String::from_utf8(whole).unwrap(),
            body,
            "nothing lost or glued"
        );
    }

    #[test]
    fn lines_become_days_and_ranked_files() {
        let found = parse("2026-10-22\tui.rs\n2026-10-22\tui.rs\n2026-10-21\ta.rs\nnonsense\n");
        let days = tally(found);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].1.total, 2);
        assert_eq!(days[0].1.files, vec![("ui.rs".to_string(), 2)]);
    }

    /// Heat of one day built out of `saves` spread over `files`.
    fn ramp(files: &[&str], saves: usize) -> usize {
        let day = Day::from_date(2026, 1, 1);
        let mut history = History::default();
        for index in 0..saves {
            history.record(day, files[index % files.len()]);
        }
        history.level(day)
    }

    #[test]
    fn the_ramp_counts_saves_and_the_files_they_spread_over() {
        assert_eq!(ramp(&["a"], 0), 0);
        assert_eq!(ramp(&["a"], 1), 1, "one Ctrl+S already counts");
        assert_eq!(ramp(&["a"], 9), 1);
        assert_eq!(ramp(&["a"], 10), 2);
        assert_eq!(ramp(&["a"], 49), 2, "the file count only lifts at fifty");
        assert_eq!(ramp(&["a"], 50), 3, "fifty saves on one file");
        assert_eq!(ramp(&["a", "b"], 60), 3, "still under three files");
        assert_eq!(ramp(&["a", "b", "c"], 60), 4, "three to ten files");
        let many: Vec<String> = (1..=12).map(|index| format!("f{index}.rs")).collect();
        let names: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(ramp(&names, 60), 5, "and more than ten files");
    }

    #[test]
    fn a_recorded_save_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.log");
        let day = Day::from_date(2026, 10, 22);
        {
            let mut probe = Probe::new(Some(path.clone()));
            probe.record(day, "E:\\Think\\x.rs");
        }
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "2026-10-22\tE:\\Think\\x.rs\n");

        let mut probe = Probe::new(Some(path));
        for _ in 0..50 {
            if probe.poll() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(probe.total(day), 1);
        assert_eq!(probe.files(day)[0].0, "E:\\Think\\x.rs");
    }
}
