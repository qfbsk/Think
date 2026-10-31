//! Syntax highlighting: tree-house (Helix's tree-sitter highlighter) driving Helix's
//! curated query corpus, coloured by the Catppuccin Mocha theme file. kite owns only
//! the plumbing: the language table, the scope registry and byte-range spans.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use glyphon::Color;
use ropey::Rope;
use tree_house::highlighter::{Highlight, Highlighter};
use tree_house::tree_sitter::{Grammar, Node};
use tree_house::{InjectionLanguageMarker, Language, LanguageConfig, LanguageLoader, Syntax};

/// Trees cost ~69x the source on this machine, so documents above this size are
/// left uncoloured rather than parsed.
pub const MAX_HIGHLIGHT_BYTES: usize = 2 * 1024 * 1024;

/// Keystrokes pay for the parser out of this; a change that cannot be folded
/// into the tree inside it keeps the colours it has and waits for the next one.
pub const TIMEOUT: Duration = Duration::from_millis(500);

/// Opening a file may freeze for a second, which is the price of the whole
/// first screen arriving coloured — a cold parse that runs out of time hands
/// back nothing, not a half-painted tree.
const PARSE_TIMEOUT: Duration = Duration::from_secs(1);

struct Definition {
    name: &'static str,
    extensions: &'static [&'static str],
    grammar: fn() -> Option<Grammar>,
    highlights: &'static str,
    injections: &'static str,
    locals: &'static str,
}

const DEFINITIONS: &[Definition] = &[
    Definition {
        name: "c",
        extensions: &["c", "h"],
        grammar: || Grammar::try_from(tree_sitter_c::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/c/highlights.scm"),
        injections: include_str!("../runtime/queries/c/injections.scm"),
        locals: include_str!("../runtime/queries/c/locals.scm"),
    },
    Definition {
        name: "cpp",
        // `.ino` is an Arduino sketch: same grammar, only `setup()`/`loop()`
        // and the pin helpers are prelude sugar the C++ grammar parses fine.
        extensions: &["cpp", "cxx", "cc", "hpp", "hh", "hxx", "ino"],
        grammar: || Grammar::try_from(tree_sitter_cpp::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/cpp/highlights.scm"),
        injections: include_str!("../runtime/queries/cpp/injections.scm"),
        locals: include_str!("../runtime/queries/cpp/locals.scm"),
    },
    Definition {
        name: "rust",
        extensions: &["rs"],
        grammar: || Grammar::try_from(tree_sitter_rust::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/rust/highlights.scm"),
        injections: include_str!("../runtime/queries/rust/injections.scm"),
        locals: include_str!("../runtime/queries/rust/locals.scm"),
    },
    Definition {
        name: "powershell",
        extensions: &["ps1", "psm1", "psd1"],
        grammar: || Grammar::try_from(tree_sitter_powershell::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/powershell/highlights.scm"),
        injections: include_str!("../runtime/queries/powershell/injections.scm"),
        locals: "",
    },
    Definition {
        name: "json",
        extensions: &["json", "jsonc"],
        grammar: || Grammar::try_from(tree_sitter_json::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/json/highlights.scm"),
        injections: include_str!("../runtime/queries/json/injections.scm"),
        locals: "",
    },
    Definition {
        name: "toml",
        extensions: &["toml"],
        grammar: || Grammar::try_from(tree_sitter_toml_ng::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/toml/highlights.scm"),
        injections: include_str!("../runtime/queries/toml/injections.scm"),
        locals: "",
    },
    Definition {
        name: "markdown",
        extensions: &["md", "markdown"],
        grammar: || Grammar::try_from(tree_sitter_md::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/markdown/highlights.scm"),
        injections: include_str!("../runtime/queries/markdown/injections.scm"),
        locals: "",
    },
    Definition {
        name: "python",
        extensions: &["py", "pyi"],
        grammar: || Grammar::try_from(tree_sitter_python::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/python/highlights.scm"),
        injections: include_str!("../runtime/queries/python/injections.scm"),
        locals: include_str!("../runtime/queries/python/locals.scm"),
    },
    Definition {
        name: "bash",
        extensions: &["sh", "bash", "ksh", "zsh"],
        grammar: || Grammar::try_from(tree_sitter_bash::LANGUAGE).ok(),
        highlights: include_str!("../runtime/queries/bash/highlights.scm"),
        injections: include_str!("../runtime/queries/bash/injections.scm"),
        locals: include_str!("../runtime/queries/bash/locals.scm"),
    },
];

pub struct Engine {
    configs: Vec<OnceLock<Option<LanguageConfig>>>,
    /// Highlight index -> dotted scope name, in the order the theme lists them.
    pub scopes: Vec<String>,
    colors: Vec<Color>,
    pair: Color,
}

impl Engine {
    pub fn new() -> Self {
        let (scopes, colors) = load_theme(include_str!("../runtime/themes/catppuccin_mocha.toml"));
        let found = |scope: &str| {
            best_scope(&scopes, scope)
                .map(|index| colors[index])
                .expect("scope in the bundled theme")
        };
        let pair = found("bracket.match");
        Self {
            configs: (0..DEFINITIONS.len()).map(|_| OnceLock::new()).collect(),
            scopes,
            colors,
            pair,
        }
    }

    pub fn color(&self, highlight: Highlight) -> Color {
        self.colors[highlight.idx().min(self.colors.len() - 1)]
    }

    /// The colour the matched bracket pair is drawn in.
    pub fn pair_color(&self) -> Color {
        self.pair
    }

    fn definition(language: Language) -> &'static Definition {
        &DEFINITIONS[language.0 as usize]
    }

    /// The cold parse happens once per document; the caller keeps the tree and feeds
    /// it `InputEdit`s from its own change records.
    pub fn parse(&self, language: Language, rope: &Rope) -> Option<Syntax> {
        Syntax::new(rope.slice(..), language, PARSE_TIMEOUT, self).ok()
    }

    /// Coloured runs over a byte range. The tree is already parsed, so this only pays
    /// for query evaluation over that range.
    pub fn runs(&self, syntax: &Syntax, rope: &Rope, from: usize, to: usize) -> Vec<Run> {
        let to = (to as u32).min(rope.len_bytes() as u32);
        if from as u32 >= to {
            return Vec::new();
        }
        let source = rope.slice(..);
        let mut highlighter = Highlighter::new(syntax, source, self, from as u32..to);
        let mut runs = Vec::new();
        let mut position = from as u32;
        let mut active: Option<Highlight> = None;
        loop {
            let next = highlighter.next_event_offset();
            if next == u32::MAX || next >= to {
                break;
            }
            if next > position {
                if let Some(highlight) = active {
                    runs.push(Run {
                        start: position as usize,
                        end: next as usize,
                        color: self.color(highlight),
                    });
                }
                position = next;
            }
            let (_, highlights) = highlighter.advance();
            active = highlights.last();
        }
        if let Some(highlight) = active {
            runs.push(Run {
                start: position as usize,
                end: to as usize,
                color: self.color(highlight),
            });
        }
        runs
    }
}

impl LanguageLoader for Engine {
    /// Markers must match exactly: a prefix match would resolve `comment` to `c` and
    /// let an injection feed itself.
    fn language_for_marker(&self, marker: InjectionLanguageMarker) -> Option<Language> {
        let text = match marker {
            InjectionLanguageMarker::Name(name) => name.to_ascii_lowercase(),
            InjectionLanguageMarker::Match(slice) | InjectionLanguageMarker::Filename(slice) => {
                let raw: String = slice.chars().take(64).collect();
                let raw = raw.to_ascii_lowercase();
                match marker {
                    InjectionLanguageMarker::Filename(_) => {
                        raw.rsplit('.').next().unwrap_or("").to_string()
                    }
                    _ => raw.trim().to_string(),
                }
            }
            _ => return None,
        };
        DEFINITIONS
            .iter()
            .position(|def| {
                def.name == text
                    || match marker {
                        InjectionLanguageMarker::Match(_)
                        | InjectionLanguageMarker::Filename(_) => {
                            def.extensions.contains(&text.as_str())
                        }
                        _ => false,
                    }
            })
            .map(|index| Language(index as u32))
    }

    fn get_config(&self, language: Language) -> Option<&LanguageConfig> {
        if language.0 as usize >= self.configs.len() {
            return None;
        }
        self.configs[language.0 as usize]
            .get_or_init(|| {
                let definition = Self::definition(language);
                let grammar = (definition.grammar)()?;
                let pick = |name: &str, field: Field| -> String {
                    DEFINITIONS
                        .iter()
                        .find(|def| def.name == name)
                        .map(|def| match field {
                            Field::Highlights => def.highlights.to_string(),
                            Field::Injections => def.injections.to_string(),
                            Field::Locals => def.locals.to_string(),
                        })
                        .unwrap_or_default()
                };
                let resolve =
                    |field| tree_house::read_query(definition.name, |name| pick(name, field));
                let config = LanguageConfig::new(
                    grammar,
                    &resolve(Field::Highlights),
                    &resolve(Field::Injections),
                    &resolve(Field::Locals),
                )
                .ok()?;
                let scopes = &self.scopes;
                config.configure(|capture| {
                    best_scope(scopes, capture).map(|index| Highlight::new(index as u32))
                });
                Some(config)
            })
            .as_ref()
    }
}

#[derive(Clone, Copy)]
enum Field {
    Highlights,
    Injections,
    Locals,
}

/// Helix's rule: a capture takes the colour of the longest theme scope it starts with.
fn best_scope(scopes: &[String], capture: &str) -> Option<usize> {
    let parts: Vec<&str> = capture.split('.').collect();
    let mut best = None;
    let mut best_len = 0usize;
    for (index, scope) in scopes.iter().enumerate() {
        let mut length = 0usize;
        let mut matches = true;
        for (position, part) in scope.split('.').enumerate() {
            match parts.get(position) {
                Some(found) if *found == part => length += 1,
                _ => {
                    matches = false;
                    break;
                }
            }
        }
        if matches && length > best_len {
            best = Some(index);
            best_len = length;
        }
    }
    best
}

fn load_theme(text: &str) -> (Vec<String>, Vec<Color>) {
    let value: toml::Value = toml::from_str(text).expect("theme toml");
    let table = value.as_table().expect("theme table");
    let palette = table
        .get("palette")
        .and_then(|value| value.as_table())
        .cloned()
        .unwrap_or_default();
    let resolve = |name: &str| -> Option<Color> {
        let named = palette
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or(name);
        hex(named)
    };
    let mut scopes = Vec::new();
    let mut colors = Vec::new();
    for (key, value) in table {
        if key == "palette" || key.starts_with("ui.") || key == "inherits" {
            continue;
        }
        let foreground = match value {
            toml::Value::String(name) => Some(name.as_str()),
            toml::Value::Table(entries) => entries
                .get("fg")
                .and_then(|value| value.as_str())
                .or_else(|| value.get("bg").and_then(|value| value.as_str())),
            _ => None,
        };
        if let Some(color) = foreground.and_then(resolve) {
            scopes.push(key.clone());
            colors.push(color);
        }
    }
    // Longest-first so a specific scope wins over its parent when both are listed.
    let order = |a: &String, b: &String| {
        a.split('.')
            .count()
            .cmp(&b.split('.').count())
            .reverse()
            .then_with(|| a.cmp(b))
    };
    let mut pairs: Vec<(String, Color)> = scopes.into_iter().zip(colors).collect();
    pairs.sort_by(|a, b| order(&a.0, &b.0));
    (
        pairs.iter().map(|(name, _)| name.clone()).collect(),
        pairs.into_iter().map(|(_, color)| color).collect(),
    )
}

fn hex(value: &str) -> Option<Color> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&digits[range], 16).ok();
    Some(Color::rgb(byte(0..2)?, byte(2..4)?, byte(4..6)?))
}

/// One coloured run: byte range in the document plus its theme colour.
#[derive(Clone, Copy)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub color: Color,
}

pub fn language_for_path(path: &Path) -> Option<Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    DEFINITIONS
        .iter()
        .position(|def| def.extensions.contains(&extension.as_str()))
        .map(|index| Language(index as u32))
}

/// The name a language answers to, the same one its query files are filed
/// under. Completion keeps its tables by this rather than by an index that
/// shifts whenever a grammar is added.
pub fn language_name(language: Language) -> &'static str {
    DEFINITIONS[language.0 as usize].name
}

pub fn enabled(bytes: usize) -> bool {
    bytes <= MAX_HIGHLIGHT_BYTES
}

/// The bracket pair the caret sits to the right of, as byte ranges.
///
/// Both halves come from the parse tree, so a bracket inside a string or a comment is
/// not a token and cannot pair, and nesting resolves through the grammar instead of a
/// depth counter. Returns `None` when the caret does not sit immediately right of a
/// bracket the grammar can pair.
pub fn bracket_pair(syntax: &Syntax, caret: usize) -> Option<((usize, usize), (usize, usize))> {
    let token = exact_token(syntax, caret.checked_sub(1)?)?;
    let (want, forward) = match token.kind() {
        "(" => (")", true),
        "[" => ("]", true),
        "{" => ("}", true),
        ")" => ("(", false),
        "]" => ("[", false),
        "}" => ("{", false),
        _ => return None,
    };
    let partner = sibling(&token, want, forward)?;
    let span = |node: &Node| (node.start_byte() as usize, node.end_byte() as usize);
    Some((span(&token), span(&partner)))
}

/// The smallest node that is exactly the one byte at `byte`, i.e. a single token.
fn exact_token(syntax: &Syntax, byte: usize) -> Option<Node<'_>> {
    let byte = byte as u32;
    let token = syntax.descendant_for_byte_range(byte, byte)?;
    (token.start_byte() == byte && token.end_byte() == byte + 1).then_some(token)
}

fn sibling<'a>(token: &Node<'a>, want: &str, forward: bool) -> Option<Node<'a>> {
    let mut node = if forward {
        token.next_sibling()
    } else {
        token.prev_sibling()
    };
    while let Some(current) = node {
        if current.kind() == want {
            return Some(current);
        }
        node = if forward {
            current.next_sibling()
        } else {
            current.prev_sibling()
        };
    }
    None
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color_name(color: Color) -> String {
        format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
    }

    fn highlight(text: &str, extension: &str, needle: &str) -> String {
        let engine = Engine::new();
        let path = Path::new("file").join(format!("probe.{extension}"));
        let language = language_for_path(&path).expect("language");
        let rope = Rope::from_str(text);
        let syntax = engine.parse(language, &rope).expect("parse");
        for run in engine.runs(&syntax, &rope, 0, rope.len_bytes()) {
            if rope.slice(run.start..run.end).to_string().contains(needle) {
                return color_name(run.color);
            }
        }
        "!uncoloured".to_string()
    }

    /// `|` marks the caret; the pair is returned as the two bracket texts.
    fn pair_at(text: &str, extension: &str) -> Option<(String, String)> {
        let caret = text.find('|').expect("caret marker");
        let source = text.replacen('|', "", 1);
        let engine = Engine::new();
        let path = Path::new("file").join(format!("probe.{extension}"));
        let language = language_for_path(&path).expect("language");
        let rope = Rope::from_str(&source);
        let syntax = engine.parse(language, &rope).expect("parse");
        bracket_pair(&syntax, caret).map(|(left, right)| {
            (
                rope.slice(left.0..left.1).to_string(),
                rope.slice(right.0..right.1).to_string(),
            )
        })
    }

    #[test]
    fn every_bundled_grammar_and_query_loads() {
        let engine = Engine::new();
        let mut broken = Vec::new();
        for index in 0..DEFINITIONS.len() {
            let definition = &DEFINITIONS[index];
            if engine.get_config(Language(index as u32)).is_some() {
                continue;
            }
            let reason = match (definition.grammar)() {
                None => "grammar refused to load".to_string(),
                Some(grammar) => {
                    let pick = |name: &str| -> String {
                        DEFINITIONS
                            .iter()
                            .find(|def| def.name == name)
                            .map(|def| def.highlights.to_string())
                            .unwrap_or_default()
                    };
                    let query = tree_house::read_query(definition.name, pick);
                    match LanguageConfig::new(
                        grammar,
                        &query,
                        definition.injections,
                        definition.locals,
                    ) {
                        Ok(_) => "unknown".to_string(),
                        Err(error) => format!("{error:?}"),
                    }
                }
            };
            broken.push(format!("{}: {reason}", definition.name));
        }
        assert!(
            broken.is_empty(),
            "unusable languages:
{}",
            broken.join(
                "
"
            )
        );
    }

    #[test]
    fn one_theme_colours_every_language() {
        // The same construct must land on the same Catppuccin colour everywhere.
        let string = "#a6e3a1";
        let comment = "#9399b2";
        assert_eq!(
            highlight("const char *s = \"alpha\";\n", "c", "alpha"),
            string
        );
        assert_eq!(highlight("let s = \"alpha\";\n", "rs", "alpha"), string);
        assert_eq!(highlight("{ \"s\": \"alpha\" }\n", "json", "alpha"), string);
        assert_eq!(highlight("s = \"alpha\"\n", "py", "alpha"), string);
        assert_eq!(highlight("# note\n", "sh", "note"), comment);
        assert_eq!(highlight("// note\n", "c", "note"), comment);
        assert_eq!(highlight("// note\n", "rs", "note"), comment);
    }

    #[test]
    fn keywords_and_numbers_get_their_own_roles() {
        assert_eq!(
            highlight("int main() { return 0; }\n", "c", "int"),
            "#cba6f7"
        );
        assert_eq!(highlight("int main() { return 0; }\n", "c", "0"), "#fab387");
        assert_eq!(highlight("fn main() {}\n", "rs", "fn"), "#cba6f7");
    }

    #[test]
    fn unknown_extensions_stay_uncoloured() {
        assert!(language_for_path(Path::new("x.unknownext")).is_none());
        assert!(language_for_path(Path::new("noextension")).is_none());
    }

    /// A sketch is C++ with a prelude — `setup()`, `loop()` and the pin helpers
    /// are ordinary calls to the grammar, so one definition colours both.
    #[test]
    fn an_arduino_sketch_is_read_as_cpp() {
        let sketch = "void setup() {\n  pinMode(13, OUTPUT);\n}\nvoid loop() {\n  delay(500);\n}\n";
        assert_eq!(highlight(sketch, "ino", "void"), "#cba6f7");
        assert_eq!(highlight(sketch, "ino", "500"), "#fab387");
        assert_eq!(highlight("// note\n", "ino", "note"), "#9399b2");
    }

    /// Two megabytes is the largest file the budget keeps, and it is the one
    /// the budget is for: a cold parse that runs out of time hands back
    /// nothing, so the whole screen would arrive colourless.
    #[test]
    fn the_largest_document_kept_is_coloured_by_the_time_it_is_given() {
        use std::time::Instant;

        let unit = include_str!("../tests/corpus/compare.c");
        let text: String = unit.repeat(2 * 1024 * 1024 / unit.len() + 1);
        assert!(text.len() >= MAX_HIGHLIGHT_BYTES);
        assert!(enabled(MAX_HIGHLIGHT_BYTES) && !enabled(MAX_HIGHLIGHT_BYTES + 1));

        let engine = Engine::new();
        let language = language_for_path(Path::new("file.c")).expect("c");
        let rope = Rope::from_str(&text);
        let started = Instant::now();
        // Running out of budget is what this test is watching for: a cold parse
        // that does is not a half-coloured document, it is no document at all.
        let syntax = engine
            .parse(language, &rope)
            .expect("the largest kept document fits its parse budget");
        let parsed = started.elapsed();
        let runs = engine.runs(&syntax, &rope, 0, 64);

        println!("cold parse of {}MB: {parsed:?}", text.len() / 1024 / 1024);
        assert!(
            runs.iter()
                .any(|run| rope.slice(run.start..run.end).to_string() == "int"),
            "the first screen came back uncoloured"
        );
    }

    #[test]
    fn the_pair_left_of_the_caret_matches_through_the_tree() {
        assert_eq!(pair_at("f(g(x)|)", "c"), Some((")".into(), "(".into())));
        assert_eq!(pair_at("f(g(x))|", "c"), Some((")".into(), "(".into())));
        assert_eq!(pair_at("int a[|x]", "c"), Some(("[".into(), "]".into())));
        assert_eq!(pair_at("int main() {|", "c"), None);
        assert_eq!(
            pair_at("fn main() {|}", "rs"),
            Some(("{".into(), "}".into()))
        );
    }

    #[test]
    fn a_bracket_inside_a_string_is_not_a_pair() {
        assert_eq!(pair_at("printf(\"(\"|)", "c"), None);
        assert_eq!(pair_at("f(|", "c"), None);
    }

    #[test]
    fn the_match_colour_comes_from_the_theme() {
        assert_eq!(color_name(Engine::new().pair_color()), "#f9e2af");
    }
}
