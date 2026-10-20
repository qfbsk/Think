use std::sync::Arc;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use glyphon::{FontSystem, SwashCache};
use kite_core::{Position, Selection};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Fullscreen, ResizeDirection, Window, WindowButtons, WindowLevel};

use crate::fonts::Fonts;
use crate::renderer::Renderer;
use crate::ui::{Focus, Mode, Ui};

const BLINK_INTERVAL: Duration = Duration::from_millis(500);
const SHELL_POLL_INTERVAL: Duration = Duration::from_millis(16);
const WHEEL_LINES: f32 = 3.0;
const VERTICAL_DELAY: Duration = Duration::from_millis(320);
const VERTICAL_INTERVAL: Duration = Duration::from_millis(90);
const VERTICAL_RAMP_SECS: f32 = 1.0;
const VERTICAL_MAX_SPEED: f32 = 3.0;

enum Drag {
    Window {
        origin: PhysicalPosition<i32>,
        cursor: PhysicalPosition<i32>,
    },
    Editor(Position),
    Shell(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VerticalDir {
    Up,
    Down,
}

impl VerticalDir {
    fn from_key(key: &Key) -> Option<Self> {
        match key {
            Key::Named(NamedKey::ArrowUp) => Some(Self::Up),
            Key::Named(NamedKey::ArrowDown) => Some(Self::Down),
            _ => None,
        }
    }

    fn step(self) -> isize {
        match self {
            Self::Up => -1,
            Self::Down => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VerticalTarget {
    Sidebar,
    Editor,
    Shell,
}

#[derive(Clone, Copy)]
struct VerticalRepeat {
    direction: VerticalDir,
    target: VerticalTarget,
    started: Instant,
    next_at: Instant,
}

pub struct App {
    fonts: Option<Fonts>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    ui: Option<Ui>,
    swash: SwashCache,
    clipboard: Option<Clipboard>,
    modifiers: ModifiersState,
    drag: Option<Drag>,
    next_blink: Instant,
    vertical: Option<VerticalRepeat>,
    ime_recent: Option<(Instant, String)>,
    ime_allowed: bool,
    fullscreen: bool,
    cursor_icon: CursorIcon,
}

impl App {
    pub fn new() -> Self {
        Self {
            fonts: None,
            window: None,
            renderer: None,
            ui: None,
            swash: SwashCache::new(),
            clipboard: None,
            modifiers: ModifiersState::empty(),
            drag: None,
            next_blink: Instant::now() + BLINK_INTERVAL,
            vertical: None,
            ime_recent: None,
            ime_allowed: true,
            fullscreen: false,
            cursor_icon: CursorIcon::Default,
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        self.fullscreen = !self.fullscreen;
        self.drag = None;
        if self.fullscreen {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            if !self.ui.as_ref().is_some_and(|ui| ui.topmost) {
                window.set_window_level(WindowLevel::AlwaysOnTop);
            }
        } else {
            window.set_fullscreen(None);
            if !self.ui.as_ref().is_some_and(|ui| ui.topmost) {
                window.set_window_level(WindowLevel::Normal);
            }
        }
    }

    fn shutdown(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(ui) = self.ui.as_mut() {
            ui.kill_shells();
        }
        event_loop.exit();
    }

    fn draw(&mut self) {
        let App {
            fonts,
            window,
            renderer,
            ui,
            swash,
            ..
        } = self;
        let (Some(fonts), Some(window), Some(renderer), Some(ui)) = (
            fonts.as_mut(),
            window.as_ref(),
            renderer.as_mut(),
            ui.as_mut(),
        ) else {
            return;
        };

        ui.prepare(&mut fonts.system);

        if let Some(area) = ui.ime_area() {
            window.set_ime_cursor_area(
                PhysicalPosition::new(area.x as i32, area.y as i32),
                PhysicalSize::new(area.w.max(1.0) as u32, area.h.max(1.0) as u32),
            );
        }

        let background = ui.background_quads();
        let foreground = ui.foreground_quads();
        let areas = ui.text_areas();
        renderer.render(&mut fonts.system, swash, &background, &foreground, areas);
    }

    fn poll_and_blink(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let App {
            fonts,
            window,
            ui,
            next_blink,
            vertical,
            ..
        } = self;
        let (Some(fonts), Some(window), Some(ui)) = (fonts.as_mut(), window.as_ref(), ui.as_mut())
        else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };

        if let Some(repeat) = *vertical {
            if now >= repeat.next_at {
                if vertical_target(ui) == Some(repeat.target) {
                    vertical_step(ui, &mut fonts.system, repeat.target, repeat.direction);
                    ui.cursor_blink_reset();
                    *next_blink = now + BLINK_INTERVAL;
                    let elapsed = now.saturating_duration_since(repeat.started + VERTICAL_DELAY);
                    if let Some(repeat) = vertical.as_mut() {
                        repeat.next_at = now + vertical_interval(elapsed);
                    }
                    window.request_redraw();
                } else {
                    *vertical = None;
                }
            }
        }

        let blink_active = ui.focus == Focus::Right && ui.mode == Mode::Insert;
        if blink_active && now >= *next_blink {
            ui.toggle_cursor_blink();
            *next_blink = now + BLINK_INTERVAL;
            window.request_redraw();
        }

        let mut deadline = if blink_active {
            Some(*next_blink)
        } else {
            None
        };

        if let Some(repeat) = vertical.as_ref() {
            let at = repeat.next_at;
            deadline = Some(deadline.map_or(at, |value| value.min(at)));
        }

        if ui.any_shell_running() {
            if ui.poll_shell(&mut fonts.system) {
                window.request_redraw();
            }
            let poll_at = now + SHELL_POLL_INTERVAL;
            deadline = Some(deadline.map_or(poll_at, |value| value.min(poll_at)));
        }

        match deadline {
            Some(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    deadline.max(now + Duration::from_millis(1)),
                ));
            }
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn handle_ime_commit(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let now = Instant::now();
        self.ime_recent = Some((now, text.clone()));
        self.next_blink = now + BLINK_INTERVAL;
        let App {
            fonts, ui, window, ..
        } = self;
        let Some(fonts) = fonts.as_mut() else {
            return;
        };
        let Some(ui) = ui.as_mut() else {
            return;
        };
        if ui.focus != Focus::Right || ui.mode != Mode::Insert {
            return;
        }
        let filtered: String = text.chars().filter(|value| !value.is_control()).collect();
        if filtered.is_empty() {
            return;
        }
        insert_into_ui(ui, &mut fonts.system, &filtered);
        ui.cursor_blink_reset();
        if let Some(window) = window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            if let Some(direction) = VerticalDir::from_key(&event.logical_key) {
                if self
                    .vertical
                    .as_ref()
                    .is_some_and(|repeat| repeat.direction == direction)
                {
                    self.vertical = None;
                }
            }
            return;
        }
        let ctrl = self.modifiers.control_key() && !self.modifiers.alt_key();
        let KeyEvent {
            logical_key: key,
            text,
            repeat,
            ..
        } = event;

        if VerticalDir::from_key(&key).is_none() {
            self.vertical = None;
        }

        if matches!(key, Key::Named(NamedKey::F11)) {
            if !repeat {
                self.toggle_fullscreen();
            }
            return;
        }

        let App {
            fonts,
            window,
            ui,
            clipboard,
            next_blink,
            vertical,
            ime_recent,
            ..
        } = self;
        let (Some(fonts), Some(ui)) = (fonts.as_mut(), ui.as_mut()) else {
            return;
        };
        let system = &mut fonts.system;

        if ctrl && char_is(&key, "q") {
            ui.show_whitespace = !ui.show_whitespace;
            ui.message = Some(if ui.show_whitespace {
                "whitespace visible".to_string()
            } else {
                "whitespace hidden".to_string()
            });
            if let Some(window) = window.as_ref() {
                window.request_redraw();
            }
            return;
        }
        if ctrl && (char_is(&key, "+") || char_is(&key, "=")) {
            if !repeat {
                request_window_scale(window, 1.08);
            }
            return;
        }
        if ctrl && char_is(&key, "-") {
            if !repeat {
                request_window_scale(window, 1.0 / 1.08);
            }
            return;
        }

        ui.message = None;

        if let Some(direction) = VerticalDir::from_key(&key) {
            if ctrl {
                if ui.focus == Focus::Sidebar && !repeat {
                    ui.sidebar.move_selection(system, direction.step());
                    if let Some(window) = window.as_ref() {
                        window.request_redraw();
                    }
                }
                return;
            }
            if repeat {
                return;
            }
            if let Some(target) = vertical_target(ui) {
                vertical_step(ui, system, target, direction);
                let now = Instant::now();
                *vertical = Some(VerticalRepeat {
                    direction,
                    target,
                    started: now,
                    next_at: now + VERTICAL_DELAY,
                });
                ui.cursor_blink_reset();
                *next_blink = now + BLINK_INTERVAL;
                if let Some(window) = window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }

        match (ui.focus, ui.mode) {
            (Focus::Sidebar, _) => {
                if repeat {
                    return;
                }
                match &key {
                    Key::Named(NamedKey::Enter) => {
                        if let Some(path) = ui.sidebar_enter(system) {
                            if let Err(error) = ui.open_file(system, path) {
                                ui.message = Some(format!("!open: {error}"));
                            }
                        }
                    }
                    Key::Named(NamedKey::Backspace) => ui.sidebar.back(system),
                    Key::Named(NamedKey::ArrowLeft) => ui.sidebar.left(system),
                    Key::Named(NamedKey::ArrowRight) => ui.sidebar.right(system),
                    Key::Character(value) if value.as_str() == "-" => ui.toggle_sidebar(system),
                    Key::Character(value) if value.as_str() == "i" || value.as_str() == "I" => {
                        ui.focus_right_insert(system)
                    }
                    _ => {}
                }
            }
            (Focus::Right, Mode::Normal) => {
                if repeat {
                    return;
                }
                match &key {
                    Key::Character(value) => match value.as_str() {
                        "i" | "I" => ui.focus_right_insert(system),
                        "-" => ui.toggle_sidebar(system),
                        "f" | "F" => {
                            if let Some(window) = window.as_ref() {
                                let enabled = !ui.topmost;
                                window.set_window_level(if enabled {
                                    WindowLevel::AlwaysOnTop
                                } else {
                                    WindowLevel::Normal
                                });
                                ui.topmost = enabled;
                                ui.message = Some(if enabled {
                                    "always on top".to_string()
                                } else {
                                    "always on top off".to_string()
                                });
                            }
                        }
                        "q" => {
                            if ui.active_tab().is_shell() {
                                ui.message = Some("the shell tab cannot be closed".to_string());
                            } else {
                                ui.close_active_file(system);
                            }
                        }
                        value if value.len() == 1 => {
                            if let Some(digit) = value.bytes().next() {
                                if digit.is_ascii_digit() {
                                    ui.activate_digit(system, digit - b'0');
                                }
                            }
                        }
                        _ => {}
                    },
                    Key::Named(NamedKey::Escape) => ui.escape(system),
                    _ => {}
                }
            }
            (Focus::Right, Mode::Insert) => {
                if handle_insert_key(
                    window,
                    ui,
                    system,
                    clipboard,
                    &key,
                    text.as_deref(),
                    ctrl,
                    repeat,
                    next_blink,
                    ime_recent,
                ) {
                    ui.cursor_blink_reset();
                    *next_blink = Instant::now() + BLINK_INTERVAL;
                }
            }
        }

        if let Some(window) = window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_mouse_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        state: ElementState,
        button: MouseButton,
    ) {
        if state == ElementState::Pressed {
            self.vertical = None;
        }
        if button != MouseButton::Left {
            return;
        }
        if state == ElementState::Released {
            self.drag = None;
            return;
        }

        let fullscreen = self.fullscreen;
        let App {
            fonts,
            window,
            ui,
            drag,
            ..
        } = self;
        let (Some(fonts), Some(window), Some(ui)) = (fonts.as_mut(), window.as_ref(), ui.as_mut())
        else {
            return;
        };
        let system = &mut fonts.system;
        let (x, y) = ui.mouse();

        if ui.hit_window_close(x, y) {
            ui.kill_shells();
            event_loop.exit();
            return;
        }

        if !fullscreen {
            if let Some(direction) = resize_direction(ui, x, y) {
                let _ = window.drag_resize_window(direction);
                return;
            }
        }

        if let Some(index) = ui.hit_close(x, y) {
            ui.close_tab(system, index);
            window.request_redraw();
            return;
        }

        if let Some(index) = ui.hit_tab(x, y) {
            ui.activate_tab(system, index);
            ui.focus_right_insert(system);
            window.request_redraw();
            return;
        }

        if !fullscreen && ui.hit_titlebar(y) {
            *drag = begin_window_drag(window);
            return;
        }

        if ui.hit_sidebar(x, y) {
            ui.focus_sidebar(system);
            let content = ui.sidebar_content_rect();
            if let Some(row) = ui.sidebar.row_at(y - content.y) {
                ui.sidebar.select_row(system, row);
            }
            ui.message = None;
            window.request_redraw();
            return;
        }

        if ui.hit_right(x, y) {
            ui.focus_right_insert(system);
            ui.message = None;
            if ui.active_editor().is_some() {
                if let Some(position) = ui.editor_position(system, x, y) {
                    ui.set_editor_selection(system, Selection::caret(position));
                    *drag = Some(Drag::Editor(position));
                }
            } else {
                let content = ui.editor_content_rect();
                let hit = ui
                    .active_shell_mut()
                    .and_then(|shell| shell.position_at(system, x - content.x, y - content.y));
                if let Some(position) = hit {
                    if let Some(shell) = ui.active_shell_mut() {
                        shell.set_input_caret(system, position);
                    }
                    *drag = Some(Drag::Shell(position));
                }
            }
            window.request_redraw();
        }
    }

    fn handle_cursor_moved(&mut self, x: f32, y: f32) {
        let App {
            fonts,
            window,
            ui,
            drag,
            cursor_icon,
            ..
        } = self;
        let (Some(fonts), Some(ui)) = (fonts.as_mut(), ui.as_mut()) else {
            return;
        };
        ui.set_mouse(x, y);
        let system = &mut fonts.system;

        match drag {
            Some(Drag::Editor(anchor)) => {
                let content = ui.editor_content_rect();
                let cx = x.clamp(content.x, content.right() - 1.0);
                let cy = y.clamp(content.y, content.bottom() - 1.0);
                if let Some(position) = ui.editor_position(system, cx, cy) {
                    ui.set_editor_selection(system, Selection::new(*anchor, position));
                }
            }
            Some(Drag::Shell(anchor)) => {
                let content = ui.editor_content_rect();
                let cx = x.clamp(content.x, content.right() - 1.0);
                let cy = y.clamp(content.y, content.bottom() - 1.0);
                let position = ui
                    .active_shell_mut()
                    .and_then(|shell| shell.position_at(system, cx - content.x, cy - content.y));
                if let Some(position) = position {
                    if let Some(shell) = ui.active_shell_mut() {
                        shell.set_input_selection(system, *anchor, position);
                    }
                }
            }
            Some(Drag::Window { origin, cursor }) => {
                #[cfg(windows)]
                if let (Some(current), Some(window)) = (global_cursor(), window.as_ref()) {
                    let dx = current.x - cursor.x;
                    let dy = current.y - cursor.y;
                    window.set_outer_position(PhysicalPosition::new(origin.x + dx, origin.y + dy));
                }
                #[cfg(not(windows))]
                let _ = (origin, cursor);
            }
            None => {}
        }

        let icon = if resize_direction(ui, x, y).is_some() {
            resize_icon(resize_direction(ui, x, y).unwrap())
        } else if ui.hit_right(x, y) && ui.active_editor().is_some() {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if icon != *cursor_icon {
            *cursor_icon = icon;
            if let Some(window) = window.as_ref() {
                window.set_cursor(icon);
            }
        }
        if let Some(window) = window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        self.vertical = None;
        let App { fonts, ui, .. } = self;
        let (Some(fonts), Some(ui)) = (fonts.as_mut(), ui.as_mut()) else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(position) => (position.y as f32 / 50.0).clamp(-4.0, 4.0),
        };
        if lines.abs() < f32::EPSILON {
            return;
        }
        if self.modifiers.control_key() {
            let step = if lines > 0.0 { 1.0 } else { -1.0 };
            ui.adjust_body_font(&mut fonts.system, step);
        } else {
            let (x, y) = ui.mouse();
            ui.scroll(&mut fonts.system, x, y, lines * WHEEL_LINES);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("kite")
            .with_decorations(false)
            .with_enabled_buttons(WindowButtons::CLOSE)
            .with_resizable(true)
            .with_min_inner_size(LogicalSize::new(640.0, 400.0))
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_visible(false);

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("kite: could not create window: {error}");
                event_loop.exit();
                return;
            }
        };

        if let Some(monitor) = window.current_monitor() {
            let screen = monitor.size();
            let frame = window.outer_size();
            let origin = monitor.position();
            if frame.width < screen.width && frame.height < screen.height {
                window.set_outer_position(PhysicalPosition::new(
                    origin.x + ((screen.width - frame.width) / 2) as i32,
                    origin.y + ((screen.height - frame.height) / 2) as i32,
                ));
            }
        }

        let renderer = pollster::block_on(Renderer::new(window.clone(), event_loop));
        let fonts = self.fonts.get_or_insert_with(Fonts::load);
        let size = window.inner_size().max(PhysicalSize::new(1, 1));
        let scale = window.scale_factor() as f32;
        let ui = Ui::new(&mut fonts.system, fonts.mono, size, scale);

        window.set_ime_allowed(true);
        window.set_cursor(CursorIcon::Default);
        window.set_visible(true);
        window.focus_window();
        window.request_redraw();

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.ui = Some(ui);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.shutdown(event_loop);
                return;
            }
            WindowEvent::Resized(size) => {
                let App {
                    fonts,
                    renderer,
                    ui,
                    ..
                } = self;
                if let Some(renderer) = renderer.as_mut() {
                    renderer.resize(size);
                }
                if let (Some(fonts), Some(ui)) = (fonts.as_mut(), ui.as_mut()) {
                    ui.resize(&mut fonts.system, size);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                return;
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer: _,
            } => {
                let Some(window) = self.window.as_ref() else {
                    return;
                };
                let size = window.inner_size();
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
                if let (Some(fonts), Some(ui)) = (self.fonts.as_mut(), self.ui.as_mut()) {
                    ui.set_scale(&mut fonts.system, scale_factor as f32, size);
                }
                window.request_redraw();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                return;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                self.vertical = None;
                return;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let is_command = event.state == ElementState::Pressed;
                self.handle_key(event);
                if is_command {
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
                return;
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.handle_ime_commit(text);
                return;
            }
            WindowEvent::Ime(_) => return,
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x as f32, position.y as f32);
                return;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(event_loop, state, button);
                return;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                return;
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.drag = None;
                    self.vertical = None;
                }
                return;
            }
            WindowEvent::CursorLeft { .. } => {
                if !matches!(self.drag, Some(Drag::Window { .. })) {
                    self.drag = None;
                }
                return;
            }
            WindowEvent::Occluded(_) => return,
            _ => {}
        }

        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.sync_ime();
        self.poll_and_blink(event_loop);
    }
}

impl App {
    fn sync_ime(&mut self) {
        let (Some(window), Some(ui)) = (self.window.as_ref(), self.ui.as_ref()) else {
            return;
        };
        let allowed = ui.focus == Focus::Right && ui.mode == Mode::Insert;
        if allowed != self.ime_allowed {
            window.set_ime_allowed(allowed);
            self.ime_allowed = allowed;
        }
    }
}

fn vertical_target(ui: &Ui) -> Option<VerticalTarget> {
    match (ui.focus, ui.mode) {
        (Focus::Sidebar, _) => Some(VerticalTarget::Sidebar),
        (Focus::Right, Mode::Insert) => {
            if ui.active_editor().is_some() {
                Some(VerticalTarget::Editor)
            } else if ui.active_shell().is_some() {
                Some(VerticalTarget::Shell)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn vertical_step(
    ui: &mut Ui,
    system: &mut FontSystem,
    target: VerticalTarget,
    direction: VerticalDir,
) {
    match target {
        VerticalTarget::Sidebar => ui.sidebar.move_selection(system, direction.step()),
        VerticalTarget::Editor => {
            if let Some(editor) = ui.active_editor_mut() {
                editor.move_vertical(system, direction == VerticalDir::Down);
            }
        }
        VerticalTarget::Shell => {
            if let Some(shell) = ui.active_shell_mut() {
                match direction {
                    VerticalDir::Up => shell.history_up(system),
                    VerticalDir::Down => shell.history_down(system),
                }
            }
        }
    }
}

fn vertical_interval(elapsed: Duration) -> Duration {
    let ramp = (elapsed.as_secs_f32() / VERTICAL_RAMP_SECS).clamp(0.0, 1.0);
    let speed = 1.0 + (VERTICAL_MAX_SPEED - 1.0) * ramp;
    VERTICAL_INTERVAL.div_f32(speed)
}

fn handle_insert_key(
    window: &mut Option<Arc<Window>>,
    ui: &mut Ui,
    system: &mut FontSystem,
    clipboard: &mut Option<Clipboard>,
    key: &Key,
    text: Option<&str>,
    ctrl: bool,
    repeat: bool,
    next_blink: &mut Instant,
    ime_recent: &mut Option<(Instant, String)>,
) -> bool {
    let mut changed = false;

    if matches!(key, Key::Named(NamedKey::Escape)) {
        if !repeat {
            ui.escape(system);
        }
        return false;
    }

    if ctrl {
        if repeat {
            return false;
        }
        match key {
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(editor) = ui.active_editor_mut() {
                    editor.move_word_left(system);
                } else if let Some(shell) = ui.active_shell_mut() {
                    shell.move_word_left(system);
                }
                changed = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(editor) = ui.active_editor_mut() {
                    editor.move_word_right(system);
                } else if let Some(shell) = ui.active_shell_mut() {
                    shell.move_word_right(system);
                }
                changed = true;
            }
            Key::Character(value) => match value.as_str() {
                "a" | "A" => {
                    if let Some(editor) = ui.active_editor_mut() {
                        editor.select_all(system);
                    } else if let Some(shell) = ui.active_shell_mut() {
                        shell.select_all(system);
                    }
                    changed = true;
                }
                "c" | "C" => {
                    if let Some(text) = selection_text(ui) {
                        if clipboard_copy(clipboard, text) {
                            ui.message = Some("copied".to_string());
                        } else {
                            ui.message = Some("!clipboard unavailable".to_string());
                        }
                    } else if let Some(shell) = ui.active_shell_mut() {
                        if shell.is_running() {
                            shell.interrupt(system);
                            changed = true;
                        }
                    }
                }
                "x" | "X" => {
                    if let Some(text) = selection_text(ui) {
                        if clipboard_copy(clipboard, text) {
                            if let Some(editor) = ui.active_editor_mut() {
                                if let Err(error) = editor.delete_selection(system) {
                                    ui.message = Some(format!("!{error}"));
                                }
                            } else if let Some(shell) = ui.active_shell_mut() {
                                shell.cut_selection(system);
                            }
                            changed = true;
                        } else {
                            ui.message = Some("!clipboard unavailable".to_string());
                        }
                    }
                }
                "v" | "V" => {
                    if let Some(text) = clipboard_paste(clipboard) {
                        paste_into_ui(ui, system, &text);
                        changed = true;
                    }
                }
                "s" | "S" => {
                    if let Err(error) = ui.save_active() {
                        ui.message = Some(format!("!{error}"));
                    }
                }
                "z" | "Z" => {
                    if let Some(editor) = ui.active_editor_mut() {
                        editor.undo(system);
                        changed = true;
                    }
                }
                "y" | "Y" => {
                    if let Some(editor) = ui.active_editor_mut() {
                        editor.redo(system);
                        changed = true;
                    }
                }
                "[" | "{" => {
                    if let Some(editor) = ui.active_editor_mut() {
                        editor.move_line_start(system);
                    } else if let Some(shell) = ui.active_shell_mut() {
                        shell.move_line_start(system);
                    }
                    changed = true;
                }
                "]" | "}" => {
                    if let Some(editor) = ui.active_editor_mut() {
                        editor.move_line_end(system);
                    } else if let Some(shell) = ui.active_shell_mut() {
                        shell.move_line_end(system);
                    }
                    changed = true;
                }
                _ => {}
            },
            _ => {}
        }
        return changed;
    }

    match key {
        Key::Named(NamedKey::Enter) => {
            let effect = if let Some(editor) = ui.active_editor_mut() {
                if let Err(error) = editor.insert_text(system, "\n") {
                    ui.message = Some(format!("!{error}"));
                }
                None
            } else if let Some(shell) = ui.active_shell_mut() {
                if shell.is_running() {
                    ui.message = Some("a job is running; Ctrl+C interrupts it".to_string());
                    None
                } else {
                    shell.submit(system)
                }
            } else {
                None
            };
            if let Some(effect) = effect {
                ui.handle_shell_effect(system, effect);
            }
            changed = true;
        }
        Key::Named(NamedKey::Backspace) => {
            if let Some(editor) = ui.active_editor_mut() {
                if let Err(error) = editor.backspace(system) {
                    ui.message = Some(format!("!{error}"));
                }
            } else if let Some(shell) = ui.active_shell_mut() {
                shell.backspace(system);
            }
            changed = true;
        }
        Key::Named(NamedKey::Delete) => {
            if let Some(editor) = ui.active_editor_mut() {
                if let Err(error) = editor.delete_forward(system) {
                    ui.message = Some(format!("!{error}"));
                }
            } else if let Some(shell) = ui.active_shell_mut() {
                shell.delete_forward(system);
            }
            changed = true;
        }
        Key::Named(NamedKey::ArrowLeft) => {
            if let Some(editor) = ui.active_editor_mut() {
                editor.move_left(system);
            } else if let Some(shell) = ui.active_shell_mut() {
                shell.move_left(system);
            }
            changed = true;
        }
        Key::Named(NamedKey::ArrowRight) => {
            if let Some(editor) = ui.active_editor_mut() {
                editor.move_right(system);
            } else if let Some(shell) = ui.active_shell_mut() {
                shell.move_right(system);
            }
            changed = true;
        }
        Key::Named(NamedKey::Tab) => {
            if let Some(editor) = ui.active_editor_mut() {
                if let Err(error) = editor.insert_text(system, "\t") {
                    ui.message = Some(format!("!{error}"));
                }
                changed = true;
            }
        }
        _ => {
            if let Some(text) = text {
                if !text.is_empty() {
                    let duplicate = ime_recent.as_ref().is_some_and(|(at, value)| {
                        value == text && at.elapsed() < Duration::from_millis(150)
                    });
                    if duplicate {
                        *ime_recent = None;
                    } else {
                        let filtered: String =
                            text.chars().filter(|value| !value.is_control()).collect();
                        if !filtered.is_empty() {
                            paste_into_ui(ui, system, &filtered);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    if changed {
        let _ = window;
        *next_blink = Instant::now() + BLINK_INTERVAL;
    }
    changed
}

fn insert_into_ui(ui: &mut Ui, system: &mut FontSystem, text: &str) {
    let error = if let Some(editor) = ui.active_editor_mut() {
        editor
            .insert_text(system, text)
            .err()
            .map(|error| error.to_string())
    } else if let Some(shell) = ui.active_shell_mut() {
        let sanitized = shell_safe(text);
        shell.insert_text(system, &sanitized);
        None
    } else {
        None
    };
    if let Some(error) = error {
        ui.message = Some(format!("!{error}"));
    }
}

fn paste_into_ui(ui: &mut Ui, system: &mut FontSystem, text: &str) {
    insert_into_ui(ui, system, text);
}

fn shell_safe(text: &str) -> String {
    text.chars()
        .map(|value| match value {
            '\r' | '\n' | '\t' => ' ',
            value => value,
        })
        .collect()
}

fn selection_text(ui: &Ui) -> Option<String> {
    if let Some(editor) = ui.active_editor() {
        return editor.selected_text();
    }
    ui.active_shell().and_then(|shell| shell.selected_text())
}

fn clipboard_copy(clipboard: &mut Option<Clipboard>, text: String) -> bool {
    if clipboard.is_none() {
        *clipboard = Clipboard::new().ok();
    }
    clipboard
        .as_mut()
        .is_some_and(|clipboard| clipboard.set_text(text).is_ok())
}

fn clipboard_paste(clipboard: &mut Option<Clipboard>) -> Option<String> {
    if clipboard.is_none() {
        *clipboard = Clipboard::new().ok();
    }
    clipboard
        .as_mut()
        .and_then(|clipboard| clipboard.get_text().ok())
}

fn char_is(key: &Key, expected: &str) -> bool {
    matches!(key, Key::Character(value) if value.as_str() == expected)
}

fn request_window_scale(window: &Option<Arc<Window>>, factor: f32) {
    let Some(window) = window.as_ref() else {
        return;
    };
    let size = window.inner_size();
    if size.width == 0 || size.height == 0 {
        return;
    }
    let scale = window.scale_factor();
    let width = (size.width as f64 / scale * factor as f64).max(640.0);
    let height = (size.height as f64 / scale * factor as f64).max(400.0);
    let _ = window.request_inner_size(LogicalSize::new(width, height));
}

fn resize_direction(ui: &Ui, x: f32, y: f32) -> Option<ResizeDirection> {
    let margin = 6.0 * ui.scale;
    let width = ui.size.width as f32;
    let height = ui.size.height as f32;
    let left = x < margin;
    let right = x > width - margin;
    let top = y < margin;
    let bottom = y > height - margin;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

fn resize_icon(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::South => CursorIcon::SResize,
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::NorthWest => CursorIcon::NwResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
    }
}

#[cfg(windows)]
fn begin_window_drag(window: &Window) -> Option<Drag> {
    let cursor = global_cursor()?;
    let origin = window.outer_position().ok()?;
    Some(Drag::Window { origin, cursor })
}

#[cfg(not(windows))]
fn begin_window_drag(window: &Window) -> Option<Drag> {
    let _ = window.drag_window();
    None
}

#[cfg(windows)]
#[repr(C)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[cfg(windows)]
fn global_cursor() -> Option<PhysicalPosition<i32>> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(point: *mut WinPoint) -> i32;
    }
    let mut point = WinPoint { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut point) } != 0 {
        Some(PhysicalPosition::new(point.x, point.y))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_repeat_accelerates_then_caps() {
        let start = vertical_interval(Duration::ZERO);
        let mid = vertical_interval(Duration::from_millis(500));
        let cap = vertical_interval(Duration::from_secs(1));
        let beyond = vertical_interval(Duration::from_secs(30));
        assert_eq!(start, VERTICAL_INTERVAL);
        assert!(mid < start);
        assert!(cap < mid);
        assert_eq!(cap, VERTICAL_INTERVAL.div_f32(VERTICAL_MAX_SPEED));
        assert_eq!(beyond, cap);
    }
}
