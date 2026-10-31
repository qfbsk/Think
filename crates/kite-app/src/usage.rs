//! What the reader has actually taken from a list, so the next list can put it
//! first. A name taken once at a project is a name this project talks about,
//! and a client that keeps that fact behaves more like a person who has been
//! reading the code than one that has not.
//!
//! The store is a plain text file under `%APPDATA%\kite`: one line per name,
//! `language <tab> name <tab> count <tab> last-used`. Nothing else reads it,
//! and a line it cannot parse is a line it drops — a corrupted counter is not
//! worth a message on screen.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::complete::Item;

/// Rows kept across all languages. Past this the oldest names are dropped when
/// the file is rewritten; a completion list has no use for the ten-thousandth.
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy)]
struct Taken {
    count: u32,
    last: u64,
}

/// The accepted names, by language and by name.
pub struct Usage {
    taken: HashMap<String, Taken>,
    path: Option<PathBuf>,
}

impl Usage {
    /// The store a window starts with. Tests get their own, because the real
    /// file holds the person's history and a test that reads it would pass or
    /// fail by what they happened to type last week.
    pub fn store() -> Self {
        if cfg!(test) {
            Self::scratch()
        } else {
            Self::load()
        }
    }

    /// The store on disk. Reading is best effort: no file is a first run.
    pub fn load() -> Self {
        let path = store_path();
        let mut taken = HashMap::new();
        if let Some(text) = path.as_ref().and_then(|path| std::fs::read_to_string(path).ok()) {
            for line in text.lines() {
                let mut fields = line.split('\t');
                let (Some(language), Some(name), Some(count), Some(last)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                let (Ok(count), Ok(last)) = (count.parse(), last.parse()) else {
                    continue;
                };
                taken.insert(key(language, name), Taken { count, last });
            }
        }
        Self { taken, path }
    }

    /// A store that lives only as long as the value, for tests.
    fn scratch() -> Self {
        Self { taken: HashMap::new(), path: None }
    }

    /// How often this name has been taken in this language, and when it was
    /// last taken: more often first, and the more recent of two equals first.
    pub fn weight(&self, language: &str, name: &str) -> (u32, u64) {
        self.taken
            .get(&key(language, name))
            .map_or((0, 0), |taken| (taken.count, taken.last))
    }

    /// Put the names this language has taken before ahead of the ones it has
    /// not. Equality of weight keeps the server's own order, so this only ever
    /// lifts what the reader has shown a preference for.
    pub fn rank(&self, language: &str, items: &mut [Item]) {
        items.sort_by(|left, right| {
            self.weight(language, &right.name)
                .cmp(&self.weight(language, &left.name))
        });
    }

    /// One more take of this name, written down at once: a counter that only
    /// reached the disk when the window closed would forget every crash.
    pub fn take(&mut self, language: &str, name: &str) {
        let last = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let entry = self
            .taken
            .entry(key(language, name))
            .or_insert(Taken { count: 0, last });
        entry.count = entry.count.saturating_add(1);
        entry.last = last;
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut entries: Vec<(&String, &Taken)> = self.taken.iter().collect();
        entries.sort_by(|left, right| (right.1.count, right.1.last).cmp(&(left.1.count, left.1.last)));
        entries.truncate(MAX_ENTRIES);
        let text: String = entries
            .iter()
            .map(|(key, taken)| format!("{key}\t{}\t{}\n", taken.count, taken.last))
            .collect();
        let _ = std::fs::write(path, text);
    }
}

/// Language and name in one key. The language never holds a tab, and an
/// identifier never does either, so splitting on the first tab is enough.
fn key(language: &str, name: &str) -> String {
    format!("{language}\t{name}")
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".config"))
    })?;
    Some(base.join("kite").join("usage.tsv"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::complete::Kind;

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|item| item.name.as_str()).collect()
    }

    #[test]
    fn a_name_taken_before_comes_first_across_the_whole_list() {
        let mut usage = Usage::scratch();
        usage.take("c", "printf");
        usage.take("c", "printf");
        usage.take("c", "putchar");
        let mut items = vec![
            Item::new("puts", Kind::Function),
            Item::new("putchar", Kind::Function),
            Item::new("printf", Kind::Function),
        ];
        usage.rank("c", &mut items);
        assert_eq!(names(&items), vec!["printf", "putchar", "puts"]);
    }

    #[test]
    fn the_order_is_kept_where_nothing_has_been_taken() {
        let usage = Usage::scratch();
        let mut items = vec![
            Item::new("b_name", Kind::Word),
            Item::new("a_name", Kind::Word),
            Item::new("c_name", Kind::Word),
        ];
        usage.rank("c", &mut items);
        assert_eq!(names(&items), vec!["b_name", "a_name", "c_name"]);
    }

    #[test]
    fn what_was_taken_in_one_language_says_nothing_about_another() {
        let mut usage = Usage::scratch();
        usage.take("c", "print");
        let mut items = vec![Item::new("println", Kind::Function), Item::new("print", Kind::Function)];
        usage.rank("python", &mut items);
        assert_eq!(
            names(&items),
            vec!["println", "print"],
            "python has not taken `print` yet, so it keeps the server's order"
        );
    }

    #[test]
    fn the_more_recent_of_two_equals_comes_first() {
        let mut usage = Usage::scratch();
        usage.take("c", "first");
        usage.take("c", "second");
        // The clock can be coarse enough that both land in the same second.
        let stale = usage.taken.get_mut(&key("c", "first")).map(|entry| entry.last = 1);
        assert!(stale.is_some());
        let mut items = vec![Item::new("first", Kind::Word), Item::new("second", Kind::Word)];
        usage.rank("c", &mut items);
        assert_eq!(names(&items), vec!["second", "first"]);
    }

    #[test]
    fn a_store_with_no_file_is_empty_rather_than_broken() {
        let usage = Usage::scratch();
        assert_eq!(usage.weight("c", "anything"), (0, 0));
    }
}
