//! What the basic commands take.
//!
//! A command line has no language server to ask, so this table is the whole of
//! kite's knowledge about arguments. It is written from each program's own help
//! text on this machine — `gcc --help`, `gcc --help=common`, `gcc --help=warnings`
//! — and not from memory, for two reasons: the help says which options want
//! something after them and in what shape (`-o file`, `-std=c99`, `-I dir`), and
//! a table copied from a man page is a table that quietly stops matching the
//! program installed here.
//!
//! The list is deliberately the options a person types, not every option a
//! compiler accepts: a nine-row panel cannot use four thousand rows, and the long
//! tail is typed from memory anyway. Anything the table does not know falls back
//! to the disk, which is what the shell did before this file existed.

use crate::complete::{Item, Kind};

/// What an option wants written after it.
#[derive(Clone, Copy)]
pub enum Arg {
    /// A path to something that already exists, or will be created.
    File,
    /// A directory. The list shows folders only.
    Folder,
    /// A name of the program's own choosing: a library, a macro, a language.
    /// Nothing on disk answers to it.
    Word,
}

/// How the argument hangs off the option. These are the shapes gcc's own help
/// prints: `-c` alone, `-O2` and `-std=c99` written onto the option, `-o file` as
/// the next word — and `-I dir`, which is accepted both ways, so it is written
/// both ways by different people.
#[derive(Clone, Copy)]
pub enum Takes {
    Nothing,
    Attached(Arg),
    Separate(Arg),
    Both(Arg),
}

pub struct Flag {
    /// The option exactly as it is typed, separator included.
    pub option: &'static str,
    takes: Takes,
    /// The program's own sentence about it, in its own words.
    help: &'static str,
}

const fn flag(option: &'static str, help: &'static str) -> Flag {
    Flag {
        option,
        takes: Takes::Nothing,
        help,
    }
}

const fn wants(option: &'static str, arg: Arg, help: &'static str) -> Flag {
    Flag {
        option,
        takes: Takes::Separate(arg),
        help,
    }
}

const fn glued(option: &'static str, arg: Arg, help: &'static str) -> Flag {
    Flag {
        option,
        takes: Takes::Attached(arg),
        help,
    }
}

const fn both(option: &'static str, arg: Arg, help: &'static str) -> Flag {
    Flag {
        option,
        takes: Takes::Both(arg),
        help,
    }
}



/// `gcc` and `g++` are the same driver — both print `Usage: <name>.exe [options]
/// file...` and the same option list — so one table serves both.
const GCC: &[Flag] = &[
    flag("-c", "Compile and assemble, but do not link"),
    flag("-S", "Compile only; do not assemble or link"),
    flag("-E", "Preprocess only; do not compile, assemble or link"),
    flag("-v", "Display the programs invoked by the compiler"),
    flag("-###", "Like -v but options quoted and commands not executed"),
    wants("-o", Arg::File, "Place the output into <file>"),
    glued("-O", Arg::Word, "Set optimization level: -O0, -O1, -O2, -O3, -Ofast, -Os, -Og"),
    both("-I", Arg::Folder, "Add a directory to the header search path"),
    both("-L", Arg::Folder, "Add a directory to the library search path"),
    both("-l", Arg::Word, "Link against the named library"),
    both("-D", Arg::Word, "Define a macro, optionally -DNAME=VALUE"),
    both("-U", Arg::Word, "Undefine a macro"),
    flag("-g", "Produce debugging information"),
    flag("-Wall", "Enable most warning messages"),
    flag("-Wextra", "Enable some extra warning messages"),
    flag("-Werror", "Make all warnings into errors"),
    flag("-Wpedantic", "Issue all the warnings demanded by strict ISO C"),
    flag("-shared", "Create a shared library"),
    flag("-pie", "Create a dynamically linked position independent executable"),
    flag("-static", "Do not link against shared libraries"),
    flag("-pthread", "Add support for multithreading"),
    flag("-ftime-report", "Report the time consumed by each compiler phase"),
    flag("-flto", "Perform link-time optimization"),
    flag("-save-temps", "Do not delete intermediate files"),
    flag("-pipe", "Use pipes rather than intermediate files"),
    flag("-pedantic", "Issue all the warnings demanded by strict ISO C and ISO C++"),
    flag("-time", "Time the execution of each subprocess"),
    flag("--help", "Display this information"),
    flag("--version", "Display compiler version information"),
    flag("-dumpversion", "Display the version of the compiler"),
    flag("-dumpmachine", "Display the compiler's target processor"),
    flag("-print-search-dirs", "Display the directories in the compiler's search path"),
    flag("-print-libgcc-file-name", "Display the name of the compiler's companion library"),
    glued("-print-file-name=", Arg::Word, "Display the full path to library <lib>"),
    glued("-std=", Arg::Word, "Assume that the input sources are for <standard>"),
    glued("-specs=", Arg::File, "Override built-in specs with the contents of <file>"),
    glued("--sysroot=", Arg::Folder, "Use <directory> as the root for headers and libraries"),
    wants("-B", Arg::Folder, "Add a directory to the compiler's search paths"),
    wants("-x", Arg::Word, "Specify the language of the following input files"),
    glued("-Wl,", Arg::Word, "Pass comma-separated options on to the linker"),
    glued("-Wa,", Arg::Word, "Pass comma-separated options on to the assembler"),
    glued("-Wp,", Arg::Word, "Pass comma-separated options on to the preprocessor"),
];

/// The programs this file knows the arguments of.
pub fn spec(program: &str) -> Option<&'static [Flag]> {
    let name = program
        .rsplit(['/', '\\'])
        .next()?
        .trim_end_matches(".exe")
        .to_lowercase();
    match name.as_str() {
        "gcc" | "g++" | "cc" | "cxx" => Some(GCC),
        _ => None,
    }
}

/// Whether the token being typed is a place on disk or one of the program's own
/// arguments — and, for an argument, whether the option before it wants a file,
/// a folder or a name. `None` is the answer for a program this file does not
/// know: the disk is a better guess than silence.
pub fn ask(program: Option<&str>, previous: Option<&str>, token: &str) -> Option<Answer<'static>> {
    let table = spec(program?)?;
    if token.starts_with('-') {
        // A glued option carries its own argument, so `-Isr` is a path being
        // written onto the same token. Case matters here: `-L` is a library
        // search path and `-l` is a library, and they are one letter apart.
        if let Some(flag) = table.iter().find(|flag| match flag.takes {
            Takes::Attached(arg) | Takes::Both(arg) => {
                matches!(arg, Arg::File | Arg::Folder)
                    && token.len() > flag.option.len()
                    && token.starts_with(flag.option)
            }
            _ => false,
        }) {
            let arg = match flag.takes {
                Takes::Attached(arg) | Takes::Both(arg) => arg,
                _ => unreachable!(),
            };
            return Some(Answer::Glued {
                option: flag.option,
                arg,
            });
        }
        return Some(Answer::Options(table));
    }
    let previous = previous?;
    let option = table.iter().find(|flag| flag.option == previous)?;
    match option.takes {
        Takes::Separate(Arg::File) | Takes::Both(Arg::File) => Some(Answer::Files),
        Takes::Separate(Arg::Folder) | Takes::Both(Arg::Folder) => Some(Answer::Folders),
        Takes::Separate(_) | Takes::Both(_) => Some(Answer::Nothing),
        // A glued or assigned argument is written on the option's own token, so
        // the word after it is a fresh argument — usually the next source file.
        _ => None,
    }
}

pub enum Answer<'a> {
    /// The program's own options, narrowed to what has been typed.
    Options(&'a [Flag]),
    /// A path being written onto the option's own token: `-Isrc/`.
    Glued { option: &'static str, arg: Arg },
    Files,
    Folders,
    /// A name of the program's own: no file list belongs here.
    Nothing,
}

/// The options a table offers for the token being typed. The row is the option and
/// the note is the program's own sentence about it, because an option list without
/// descriptions is a list the reader has to already know the answer to.
pub fn rows(table: &[Flag], token: &str) -> Vec<Item> {
    let folded = token.to_lowercase();
    table
        .iter()
        .filter(|flag| flag.option.to_lowercase().starts_with(&folded))
        .map(|flag| {
            let mut item = Item::new(flag.option, Kind::Flag);
            item.note = Some(flag.help.to_string());
            item
        })
        .collect()
}
