//! The band above the status bar: `typing-visualizer.html`'s spectrum scaled
//! down to a strip and driven by a key map that is never drawn.
//!
//! Every constant and every curve here is the reference's own, so the band
//! behaves like the page at a fraction of its height: one hit per physical
//! key, a gaussian push on the bars under that key, energy that decays while
//! you type slowly and holds while you type fast.

use std::f32::consts::PI;
use std::time::Instant;

use glyphon::Color;
use winit::keyboard::KeyCode;

use crate::renderer::QuadVertex;
use crate::ui::{Rect, push_gradient_rect, push_line, push_rect};

/// Logical pixels the band is worth when the window is tall enough to keep it.
pub const STRIP_HEIGHT: f32 = 64.0;
/// Below this window height, in logical pixels, the band takes no space at all
/// and stops moving. Ten times the band's own height is the point where it is
/// still a band rather than a line drawn across the bottom of the window.
pub const STRIP_MIN_HEIGHT: f32 = STRIP_HEIGHT * 10.0;

const BAR_COUNT: usize = 110;
const WAVE_POINTS: usize = 72;
/// The resting level: the reference's floor, and the state the band stops in.
const IDLE: f32 = 0.06;
/// The height, in logical pixels, a bar has to clear before it is drawn at all.
/// A bar of a pixel or two, one every fourteen, is not a spectrum: it is a
/// dashed line across the bottom of the window, and it is exactly what the
/// band spends most of its time looking like while typing is light.
const BAR_FLOOR: f32 = 2.5;
/// Below this a bar cannot have anything left on screen, so the band may stop.
const STILL: f32 = BAR_FLOOR / (STRIP_HEIGHT * 0.42);
const ENERGY_PER_KEY: f32 = 0.13;

/// The real board's rows, as `[key, width in units]`, left to right. Only the
/// centre of each key is ever used, and only as a fraction of `BOARD_UNITS`.
const ROWS: &[&[(KeyCode, f32)]] = &[
    &[
        (KeyCode::Backquote, 1.0),
        (KeyCode::Digit1, 1.0),
        (KeyCode::Digit2, 1.0),
        (KeyCode::Digit3, 1.0),
        (KeyCode::Digit4, 1.0),
        (KeyCode::Digit5, 1.0),
        (KeyCode::Digit6, 1.0),
        (KeyCode::Digit7, 1.0),
        (KeyCode::Digit8, 1.0),
        (KeyCode::Digit9, 1.0),
        (KeyCode::Digit0, 1.0),
        (KeyCode::Minus, 1.0),
        (KeyCode::Equal, 1.0),
        (KeyCode::Backspace, 2.0),
    ],
    &[
        (KeyCode::Tab, 1.5),
        (KeyCode::KeyQ, 1.0),
        (KeyCode::KeyW, 1.0),
        (KeyCode::KeyE, 1.0),
        (KeyCode::KeyR, 1.0),
        (KeyCode::KeyT, 1.0),
        (KeyCode::KeyY, 1.0),
        (KeyCode::KeyU, 1.0),
        (KeyCode::KeyI, 1.0),
        (KeyCode::KeyO, 1.0),
        (KeyCode::KeyP, 1.0),
        (KeyCode::BracketLeft, 1.0),
        (KeyCode::BracketRight, 1.0),
        (KeyCode::Backslash, 1.5),
    ],
    &[
        (KeyCode::CapsLock, 1.75),
        (KeyCode::KeyA, 1.0),
        (KeyCode::KeyS, 1.0),
        (KeyCode::KeyD, 1.0),
        (KeyCode::KeyF, 1.0),
        (KeyCode::KeyG, 1.0),
        (KeyCode::KeyH, 1.0),
        (KeyCode::KeyJ, 1.0),
        (KeyCode::KeyK, 1.0),
        (KeyCode::KeyL, 1.0),
        (KeyCode::Semicolon, 1.0),
        (KeyCode::Quote, 1.0),
        (KeyCode::Enter, 2.25),
    ],
    &[
        (KeyCode::ShiftLeft, 2.25),
        (KeyCode::KeyZ, 1.0),
        (KeyCode::KeyX, 1.0),
        (KeyCode::KeyC, 1.0),
        (KeyCode::KeyV, 1.0),
        (KeyCode::KeyB, 1.0),
        (KeyCode::KeyN, 1.0),
        (KeyCode::KeyM, 1.0),
        (KeyCode::Comma, 1.0),
        (KeyCode::Period, 1.0),
        (KeyCode::Slash, 1.0),
        (KeyCode::ShiftRight, 2.75),
    ],
    &[
        (KeyCode::ControlLeft, 1.5),
        (KeyCode::SuperLeft, 1.25),
        (KeyCode::AltLeft, 1.25),
        (KeyCode::Space, 6.5),
        (KeyCode::AltRight, 1.25),
        (KeyCode::Fn, 1.25),
        (KeyCode::ControlRight, 1.5),
    ],
];
const BOARD_UNITS: f32 = 15.5;

fn key_map() -> Vec<(KeyCode, f32)> {
    let mut map = Vec::new();
    for row in ROWS {
        let mut x = 0.0f32;
        for (key, width) in *row {
            map.push((*key, (x + width / 2.0) / BOARD_UNITS));
            x += width;
        }
    }
    map
}

struct Bar {
    v: f32,
    tv: f32,
    pulse: f32,
    speed: f32,
    spike: f32,
}

struct Node {
    v: f32,
    tv: f32,
    phase: f32,
}

pub struct Visualizer {
    bars: Vec<Bar>,
    wave: Vec<Node>,
    energy: f32,
    keys: Vec<(KeyCode, f32)>,
    noise: u64,
    last_tick: Instant,
}

impl Default for Visualizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Visualizer {
    pub fn new() -> Self {
        let mut viz = Self {
            bars: Vec::with_capacity(BAR_COUNT),
            wave: Vec::with_capacity(WAVE_POINTS),
            energy: IDLE,
            keys: key_map(),
            noise: 0x9e37_79b9_7f4a_7c15,
            last_tick: Instant::now(),
        };
        for _ in 0..BAR_COUNT {
            let pulse = viz.random() * 2.0 * PI;
            let speed = viz.random() * 6.0 + 5.0;
            viz.bars.push(Bar {
                v: 0.02,
                tv: 0.02,
                pulse,
                speed,
                spike: 0.0,
            });
        }
        for _ in 0..WAVE_POINTS {
            let phase = viz.random() * 2.0 * PI;
            viz.wave.push(Node {
                v: 0.0,
                tv: 0.0,
                phase,
            });
        }
        viz
    }

    /// Where a key lands, as a fraction of the board's width. Keys the board
    /// doesn't carry - the F row, the arrows - land anywhere, as they do in
    /// the page.
    fn position(&self, key: KeyCode) -> Option<f32> {
        self.keys
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, x)| *x)
    }

    pub fn beat(&mut self, key: KeyCode) {
        self.energy = (self.energy + ENERGY_PER_KEY).min(1.0);
        match self.position(key) {
            Some(x) => {
                let strength = 0.5 + self.random() * 0.35;
                self.hit_at(x, strength);
            }
            None => {
                let anywhere = self.random();
                self.hit_at(anywhere, 0.4);
            }
        }
        // The decay is measured from the last frame, never from a gap the band
        // spent asleep.
        self.last_tick = Instant::now();
    }

    fn hit_at(&mut self, x: f32, strength: f32) {
        let center = x * (BAR_COUNT - 1) as f32;
        for offset in -7i32..=7 {
            let index = (center.round() as i32 + offset).clamp(0, BAR_COUNT as i32 - 1) as usize;
            let fall = -(offset * offset) as f32 / 9.0;
            let bar = &mut self.bars[index];
            bar.spike = (bar.spike + fall.exp() * strength).min(1.6);
        }
        for _ in 0..4 {
            let index = (center / BAR_COUNT as f32 * WAVE_POINTS as f32
                + (self.random() - 0.5) * 8.0)
                .round()
                .clamp(0.0, WAVE_POINTS as f32 - 1.0) as usize;
            let kick = self.random() * 0.6 + 0.2;
            self.wave[index].v += kick * strength;
        }
    }

    /// Advance the curves. Returns whether the band still has anything to show.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f32()
            .min(0.05);
        self.last_tick = now;
        self.energy = (self.energy * 0.55f32.powf(dt)).max(IDLE);
        let lift = self.lift();

        for (i, bar) in self.bars.iter_mut().enumerate() {
            let env = smooth_edges(BAR_COUNT, i);
            bar.pulse += dt * bar.speed * (0.5 + self.energy * 1.6);
            let wobble = 0.5 + 0.5 * bar.pulse.sin() * (bar.pulse * 0.63 + i as f32).sin();
            // No resting floor: a bar that is only a pixel and a half tall, one
            // every fourteen, is not a spectrum but a dashed line across the
            // bottom of the window. The bars start with the typing.
            let idle =
                env * lift * (0.25 + 0.75 * wobble) * (0.7 + 0.3 * (i as f32 * 12.9898).sin());
            bar.tv = (idle + bar.spike * 0.8).min(1.0);
            bar.spike *= 0.02f32.powf(dt);
            bar.v += (bar.tv - bar.v) * (1.0 - 0.0005f32.powf(dt));
        }

        for (i, node) in self.wave.iter_mut().enumerate() {
            node.phase += dt * (2.0 + self.energy * 9.0);
            let env = smooth_edges(WAVE_POINTS, i);
            let inside = node.phase + (node.phase * 0.37).sin() * 2.0;
            node.tv = env * self.energy * (0.35 + 0.65 * (0.5 + 0.5 * inside.sin()));
            node.v += (node.tv - node.v) * (1.0 - 0.001f32.powf(dt));
            node.v *= 0.35f32.powf(dt) * 0.2 + 0.8;
        }

        self.live()
    }

    /// Whether there is still motion to draw. Once the energy has drained and
    /// the last spike has faded, the band holds its resting shape and waits.
    /// Advance the curves. Returns whether the band still has anything to show.
    ///
    /// The bars chase their target exponentially, so the energy reaching the
    /// floor does not mean the last frame has finished fading: stop on that
    /// frame and whatever height it had is painted for good, which at a pixel
    /// or two is a dashed line across the bottom of the window.
    pub fn live(&self) -> bool {
        self.energy > IDLE + 0.0005
            || self
                .bars
                .iter()
                .any(|bar| bar.spike > 0.01 || bar.v > STILL)
    }

    /// How loud the band is right now: nothing at rest, everything at full
    /// energy. The resting level is not zero, so it is taken out here rather
    /// than at every call site. Rooted, so the first few keys already read as
    /// the band waking up rather than as a third of a keystroke of brightness.
    fn lift(&self) -> f32 {
        ((self.energy - IDLE) / (1.0 - IDLE)).max(0.0).sqrt()
    }

    /// Put the band back at rest. Called when the window gets too short for it,
    /// so a taller window later on doesn't resume a half-finished gesture.
    pub fn reset(&mut self) {
        self.energy = IDLE;
        self.last_tick = Instant::now();
        for bar in &mut self.bars {
            bar.v = 0.02;
            bar.tv = 0.02;
            bar.spike = 0.0;
        }
        for node in &mut self.wave {
            node.v = 0.0;
            node.tv = 0.0;
        }
    }

    pub fn quads(&self, strip: Rect, scale: f32) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        if strip.h <= 0.0 || strip.w <= 0.0 {
            return quads;
        }
        let height = strip.h;
        let baseline = strip.y + height * 0.5;
        let margin = strip.w * 0.02;
        let span = (strip.w - margin * 2.0).max(1.0);
        let left = strip.x + margin;
        let slot = span / BAR_COUNT as f32;
        let solid_w = (slot * 0.42).max(1.5 * scale);
        let ramp = height * 0.46;
        let floor = BAR_FLOOR * scale;

        for (i, bar) in self.bars.iter().enumerate() {
            let h = bar.v * height * 0.42;
            if h < floor {
                continue;
            }
            // A bar grows into being solid rather than appearing at full
            // strength the moment it clears the floor.
            let alpha = ((h - floor) / (height * 0.22)).min(1.0);
            let x = left + i as f32 * slot;
            let top = baseline - h;
            let glow = (46.0 + self.energy * 30.0) * alpha;
            // What the page paints as a canvas shadow becomes a wash across the
            // whole slot, so neighbouring bars bleed into one soft field.
            push_gradient_rect(
                &mut quads,
                Rect {
                    x,
                    y: top,
                    w: slot,
                    h,
                },
                ramp_color(h * 1.35 / ramp, (glow * 0.25) as u8),
                ramp_color(0.0, glow as u8),
            );
            let solid = Rect {
                x: x + (slot - solid_w) / 2.0,
                y: top,
                w: solid_w,
                h,
            };
            let solid_alpha = (255.0 * alpha) as u8;
            push_gradient_rect(
                &mut quads,
                solid,
                ramp_color(h / ramp, solid_alpha),
                ramp_color(0.0, solid_alpha),
            );
            push_rect(
                &mut quads,
                Rect {
                    x: solid.x,
                    y: baseline,
                    w: solid.w,
                    h: h * 0.55,
                },
                Color::rgba(230, 60, 230, (71.0 * alpha) as u8),
            );
        }

        let point = |i: usize| -> (f32, f32) {
            let x = left + (i as f32 / (WAVE_POINTS - 1) as f32) * span;
            (x, baseline - self.wave[i].v.max(0.0) * height * 0.30)
        };
        let line: Vec<(f32, f32)> = (0..WAVE_POINTS).map(point).collect();
        let mirrored: Vec<(f32, f32)> = line
            .iter()
            .map(|(x, y)| (*x, baseline + (baseline - y) * 0.55))
            .collect();
        // The wave and the line it stands on are the only things that reach the
        // full width of the band, so at rest they read as a stripe across the
        // window rather than as a band. They come up with the typing.
        let lift = self.lift();
        push_smooth(
            &mut quads,
            &mirrored,
            1.3 * scale,
            Color::rgba(255, 255, 255, (77.0 * lift) as u8),
            2,
        );
        push_smooth(
            &mut quads,
            &line,
            4.0 * scale,
            Color::rgba(255, 255, 255, (80.0 * lift) as u8),
            2,
        );
        push_smooth(
            &mut quads,
            &line,
            1.6 * scale,
            Color::rgba(255, 255, 255, (235.0 * lift) as u8),
            4,
        );

        // The line the bars stand on, brightest under the middle of the board.
        let peak = lift * 0.75;
        let segments = 16;
        for s in 0..segments {
            let u0 = s as f32 / segments as f32;
            let u1 = (s + 1) as f32 / segments as f32;
            let triangle = |u: f32| 1.0 - (2.0 * u - 1.0).abs();
            let alpha = ((triangle(u0) + triangle(u1)) * 0.5 * peak * 255.0) as u8;
            push_line(
                &mut quads,
                left + u0 * span,
                baseline - 0.5,
                left + u1 * span,
                baseline - 0.5,
                1.0 * scale,
                Color::rgba(255, 140, 255, alpha),
            );
        }

        quads
    }

    /// A spread over [0, 1). The band needs jitter, not cryptography.
    fn random(&mut self) -> f32 {
        self.noise ^= self.noise << 13;
        self.noise ^= self.noise >> 7;
        self.noise ^= self.noise << 17;
        (self.noise >> 11) as f32 / (1u64 << 53) as f32
    }
}

/// The page's `quadraticCurveTo` ribbon: each sample becomes the control
/// point of a curve that runs to the midpoint of its own and the next sample,
/// so 72 points read as one smooth line instead of a zigzag.
fn push_smooth(
    quads: &mut Vec<QuadVertex>,
    points: &[(f32, f32)],
    width: f32,
    color: Color,
    steps: usize,
) {
    if points.len() < 2 {
        return;
    }
    let mut cursor = points[0];
    for i in 1..points.len() {
        let start = cursor;
        let control = points[i - 1];
        let end = if i + 1 < points.len() {
            (
                (points[i - 1].0 + points[i].0) / 2.0,
                (points[i - 1].1 + points[i].1) / 2.0,
            )
        } else {
            points[i]
        };
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let u = 1.0 - t;
            let next = (
                u * u * start.0 + 2.0 * u * t * control.0 + t * t * end.0,
                u * u * start.1 + 2.0 * u * t * control.1 + t * t * end.1,
            );
            push_line(quads, cursor.0, cursor.1, next.0, next.1, width, color);
            cursor = next;
        }
        cursor = end;
    }
}

/// The page's edge falloff: the spectrum dies away at the sides of the band.
fn smooth_edges(count: usize, index: usize) -> f32 {
    let x = index as f32 / (count - 1) as f32;
    ((x * PI).sin() * 2.2).clamp(0.06, 1.0)
}

/// The page's bar gradient - `#ff1fd4` through `#e436ff` into `#8a2bff` over
/// the tallest bar it can draw - sampled at `r`.
fn ramp_color(r: f32, alpha: u8) -> Color {
    let r = r.clamp(0.0, 1.0);
    let (from, to, at) = if r <= 0.55 {
        ([0xff, 0x1f, 0xd4], [0xe4, 0x36, 0xff], r / 0.55)
    } else {
        ([0xe4, 0x36, 0xff], [0x8a, 0x2b, 0xff], (r - 0.55) / 0.45)
    };
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * at) as u8;
    Color::rgba(
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
        alpha,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_the_board_is_on_the_scale() {
        let viz = Visualizer::new();
        // Every key lands inside the band, and each row reads left to right.
        for (_, x) in &viz.keys {
            assert!(*x > 0.0 && *x < 1.0, "{x} is off the band");
        }
        let row = |keys: [KeyCode; 3]| {
            let found: Vec<f32> = keys.iter().filter_map(|key| viz.position(*key)).collect();
            assert!(
                found.windows(2).all(|pair| pair[0] < pair[1]),
                "a row runs backwards"
            );
        };
        row([KeyCode::Tab, KeyCode::KeyQ, KeyCode::Backslash]);
        row([KeyCode::CapsLock, KeyCode::KeyF, KeyCode::Enter]);
        row([KeyCode::ShiftLeft, KeyCode::KeyN, KeyCode::ShiftRight]);
        // The space bar is the widest key on the board, so it sits at its middle.
        let space = viz.position(KeyCode::Space).unwrap();
        assert!(space > 0.4 && space < 0.55, "the space bar is the middle");
        assert!(
            viz.position(KeyCode::F1).is_none(),
            "the F row is off the board"
        );
    }

    #[test]
    fn a_key_beats_once_and_the_energy_rises() {
        let mut viz = Visualizer::new();
        assert!(!viz.live(), "a band nobody has hit is asleep");
        viz.beat(KeyCode::KeyF);
        assert!(viz.live());
        assert!(viz.bars.iter().any(|bar| bar.spike > 0.0));
    }

    #[test]
    fn the_band_settles_back_to_still() {
        let mut viz = Visualizer::new();
        viz.beat(KeyCode::KeyF);
        for _ in 0..600 {
            std::thread::sleep(std::time::Duration::from_millis(6));
            viz.tick();
        }
        assert!(!viz.live(), "six seconds of nothing drains the energy");
        assert!(viz.energy <= IDLE + 0.0005);
    }

    /// A bar of a pixel or two, one every fourteen pixels, is not a spectrum:
    /// it is a dashed line across the bottom of the window. A bar has to grow
    /// into being solid.
    #[test]
    fn a_short_bar_is_never_a_solid_stripe() {
        let strip = Rect {
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: STRIP_HEIGHT,
        };
        let loudest = |quads: &[QuadVertex]| -> f32 {
            quads
                .iter()
                .map(|vertex| vertex.color[3])
                .fold(0.0f32, f32::max)
        };
        let mut viz = Visualizer::new();
        for level in [0.02, 0.05, 0.09] {
            viz.energy = IDLE;
            for bar in &mut viz.bars {
                bar.v = level;
                bar.tv = level;
                bar.spike = 0.0;
            }
            let drawn = loudest(&viz.quads(strip, 1.0));
            let tall = level * 0.42 * 100.0;
            assert!(
                drawn < 0.2,
                "a bar {tall:.0}% of the band's height paints at {drawn}"
            );
        }
    }

    #[test]
    fn fast_typing_holds_the_energy_up() {
        let mut viz = Visualizer::new();
        for _ in 0..12 {
            viz.beat(KeyCode::Space);
            std::thread::sleep(std::time::Duration::from_millis(30));
            assert!(viz.tick(), "the band is still moving between keys");
        }
        assert!(viz.energy > 0.3);
    }

    #[test]
    fn a_collapsed_band_draws_nothing_and_a_reset_is_still() {
        let mut viz = Visualizer::new();
        viz.beat(KeyCode::KeyG);
        assert!(
            viz.quads(Rect::default(), 1.0).is_empty(),
            "zero height means zero shapes"
        );
        viz.reset();
        assert!(!viz.live());
        assert!(viz.bars.iter().all(|bar| bar.spike == 0.0));
    }

    /// The wave and the line it stands on are the only shapes that run the
    /// whole width of the band, so an idle one must not be drawing them: that
    /// is a stripe across the window, not a band.
    #[test]
    fn an_idle_band_draws_no_line_across_the_window() {
        let strip = Rect {
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: STRIP_HEIGHT,
        };
        let loudest = |quads: &[QuadVertex]| -> f32 {
            quads
                .iter()
                .map(|vertex| vertex.color[3])
                .fold(0.0f32, f32::max)
        };
        let mut viz = Visualizer::new();
        for _ in 0..40 {
            viz.tick();
        }
        // A resting band must not draw a stripe across the window: a bar a
        // pixel and a half tall, one every fourteen, is a dashed line, not a
        // spectrum.
        let quiet = loudest(&viz.quads(strip, 1.0));
        // Three ordinary keystrokes are already the band waking up, not a third
        // of a keystroke of brightness.
        for key in [KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD] {
            viz.beat(key);
        }
        viz.tick();
        let three = loudest(&viz.quads(strip, 1.0));
        // A burst of typing, which is what the reference's energy is built for:
        // each key lifts it and the decay between two fast keys is nil.
        for round in 0..4 {
            for key in [
                KeyCode::KeyA,
                KeyCode::KeyS,
                KeyCode::KeyD,
                KeyCode::KeyF,
                KeyCode::KeyG,
                KeyCode::KeyH,
                KeyCode::KeyJ,
                KeyCode::KeyK,
            ] {
                viz.beat(key);
            }
            if round < 3 {
                viz.tick();
            }
        }
        viz.tick();
        let loud = loudest(&viz.quads(strip, 1.0));
        assert!(quiet < 0.01, "a resting band still draws a line at {quiet}");
        assert!(
            three > 0.4,
            "three keys have to wake the band up, got {three}"
        );
        assert!(
            loud > 0.8,
            "typing has to bring the line up to full, got {loud}"
        );
    }
}
