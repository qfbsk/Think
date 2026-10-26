#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity;
mod app;
mod archive;
mod banner;
mod day;
mod editor;
mod fonts;
mod git;
mod miku;
mod renderer;
mod shell;
mod sidebar;
mod sparks;
mod syntax;
mod terminal;
mod text;
mod theme;
mod ui;
mod visualizer;

use winit::event_loop::{ControlFlow, EventLoop};

/// A panic in a windowed build leaves no console and no WER record: the window
/// just vanishes. Whatever is written here is the only trace of such a crash.
fn record_panic(info: &std::panic::PanicHookInfo<'_>) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let backtrace = std::backtrace::Backtrace::force_capture();
    let entry = format!("{secs} {info}\n{backtrace}\n");
    eprint!("{entry}");
    let path = std::env::temp_dir().join("kite-panic.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = file.write_all(entry.as_bytes());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::panic::set_hook(Box::new(record_panic));
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = app::App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}
