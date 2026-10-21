#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod banner;
mod editor;
mod fonts;
mod git;
mod renderer;
mod shell;
mod sidebar;
mod syntax;
mod text;
mod theme;
mod ui;

use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = app::App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}
