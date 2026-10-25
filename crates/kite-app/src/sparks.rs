//! The typing sparks: a pinch of square powder thrown off the caret on every
//! keystroke. The band below reads the keyboard; this reads the caret, so it
//! lands where the typing actually goes.

use std::f32::consts::PI;
use std::time::Instant;

use glyphon::Color;

use crate::renderer::QuadVertex;
use crate::ui::{Rect, push_rect};

/// Grains per keystroke.
const GRAINS: usize = 5;
/// What the screen can hold at once: holding a key down must not grow this.
const MOST: usize = 160;
/// Logical pixels per second squared: the powder leaves the caret and drops,
/// so a keystroke kicks off the corner of a fountain rather than a ball.
const GRAVITY: f32 = 220.0;
/// How fast the throw dies out into a hang, on both axes. The cone, not the
/// damping, sets which way it leaves; damping only the sideways axis lets the
/// fast grains run several lines down before they fade.
const DRAG: f32 = 0.25;
/// The cone the powder leaves in, measured in screen angles where a quarter
/// turn is straight down: it throws along the line it was written on and
/// spills toward the next one, so the pinch reads as going to the lower right.
const UPWARD: f32 = -PI * 0.1;
const OUTWARD: f32 = PI * 0.38;

struct Grain {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    size: f32,
    age: f32,
    life: f32,
    color: Color,
}

pub struct Sparks {
    grains: Vec<Grain>,
    clock: Instant,
    noise: u64,
    scale: f32,
}

impl Sparks {
    pub fn new() -> Self {
        Self {
            grains: Vec::new(),
            clock: Instant::now(),
            noise: 0x9e37_79b9_7f4a_7c15,
            scale: 1.0,
        }
    }

    /// A keystroke at a point of the panel: powder the colour of the glyph the
    /// caret has just come off, thrown toward the lower right the way the line
    /// is already travelling.
    pub fn burst(&mut self, x: f32, y: f32, scale: f32, color: Color) {
        self.scale = scale;
        for _ in 0..GRAINS {
            let angle = UPWARD + self.random() * (OUTWARD - UPWARD);
            let speed = 45.0 + self.random() * 95.0;
            let size = (2.2 + self.random() * 1.76) * scale;
            let life = 0.45 + self.random() * 0.4;
            if self.grains.len() >= MOST {
                self.grains.remove(0);
            }
            self.grains.push(Grain {
                x,
                y,
                vx: angle.cos() * speed * scale,
                vy: angle.sin() * speed * scale,
                size,
                age: 0.0,
                life,
                color,
            });
        }
    }

    /// Advance the powder. Called from the same pump that keeps the band alive,
    /// so a still screen costs nothing.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.clock)
            .as_secs_f32()
            .min(0.05);
        self.clock = now;
        if dt <= 0.0 {
            return;
        }
        let drag = DRAG.powf(dt);
        for grain in &mut self.grains {
            grain.age += dt;
            grain.vy += GRAVITY * self.scale * dt;
            grain.x += grain.vx * dt;
            grain.y += grain.vy * dt;
            grain.vx *= drag;
            grain.vy *= drag;
            grain.vy *= drag;
        }
        self.grains.retain(|grain| grain.age < grain.life);
    }

    pub fn live(&self) -> bool {
        !self.grains.is_empty()
    }

    pub fn quads(&self) -> Vec<QuadVertex> {
        let mut quads = Vec::new();
        for grain in &self.grains {
            let left = grain.age / grain.life;
            let alpha = (1.0 - left) * (1.0 - left);
            let size = grain.size * (1.0 - 0.45 * left);
            let (r, g, b, _) = grain.color.as_rgba_tuple();
            push_rect(
                &mut quads,
                Rect {
                    x: grain.x - size / 2.0,
                    y: grain.y - size / 2.0,
                    w: size,
                    h: size,
                },
                Color::rgba(r, g, b, (255.0 * alpha) as u8),
            );
        }
        quads
    }

    /// A spread over [0, 1). The powder needs jitter, not cryptography.
    fn random(&mut self) -> f32 {
        self.noise ^= self.noise << 13;
        self.noise ^= self.noise >> 7;
        self.noise ^= self.noise << 17;
        (self.noise >> 11) as f32 / (1u64 << 53) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Color = Color::rgb(0x89, 0xb4, 0xfa);

    /// The powder is dyed with whatever the caller says the caret was sitting
    /// next to, and nothing else.
    #[test]
    fn the_powder_wears_the_ink_it_was_given() {
        let mut sparks = Sparks::new();
        sparks.burst(40.0, 20.0, 1.0, INK);
        assert_eq!(sparks.grains.len(), GRAINS);
        assert!(
            sparks.grains.iter().all(|grain| grain.color == INK),
            "a grain was dyed with some other colour"
        );
    }

    #[test]
    fn a_burst_throws_a_pinch_and_dies_back_to_nothing() {
        let mut sparks = Sparks::new();
        assert!(!sparks.live());
        sparks.burst(40.0, 20.0, 1.0, INK);
        assert_eq!(sparks.grains.len(), GRAINS);
        assert!(sparks.live());
        assert!(!sparks.quads().is_empty(), "fresh powder is drawn");
        for _ in 0..400 {
            std::thread::sleep(std::time::Duration::from_millis(6));
            sparks.tick();
        }
        assert!(
            !sparks.live(),
            "the powder has to be gone once its life is spent"
        );
        assert!(sparks.quads().is_empty());
    }

    #[test]
    fn powder_falls_and_fades_instead_of_sitting_on_the_caret() {
        let mut sparks = Sparks::new();
        sparks.burst(200.0, 100.0, 1.0, INK);
        let first = sparks
            .grains
            .iter()
            .map(|grain| (grain.x, grain.y))
            .collect::<Vec<_>>();
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(6));
            sparks.tick();
        }
        let moved = sparks
            .grains
            .iter()
            .map(|grain| (grain.x, grain.y))
            .collect::<Vec<_>>();
        assert!(
            moved
                .iter()
                .zip(&first)
                .any(|((x, _), (was, _))| (x - was).abs() > 1.0),
            "the powder has to spread sideways off the caret"
        );
        let loudest = sparks
            .quads()
            .iter()
            .map(|vertex| vertex.color[3])
            .fold(0.0f32, f32::max);
        assert!(loudest < 1.0, "mid flight it is already fading: {loudest}");
    }

    /// Powder that leaves evenly in all four directions is a firework shell.
    /// Typing throws it along the line, toward the lower right.
    #[test]
    fn the_burst_leaves_the_caret_toward_the_lower_right() {
        let mut sparks = Sparks::new();
        for _ in 0..40 {
            sparks.burst(300.0, 300.0, 1.0, INK);
        }
        let right = sparks.grains.iter().filter(|grain| grain.vx > 0.0).count();
        let down = sparks.grains.iter().filter(|grain| grain.vy > 0.0).count();
        let total = sparks.grains.len();
        assert_eq!(right, total, "some powder threw itself back left");
        assert!(
            down * 2 > total,
            "only {down} of {total} grains left downward"
        );
    }

    #[test]
    fn holding_a_key_does_not_grow_the_screen_forever() {
        let mut sparks = Sparks::new();
        for _ in 0..200 {
            sparks.burst(10.0, 10.0, 1.0, INK);
        }
        assert!(sparks.grains.len() <= MOST);
    }
}
