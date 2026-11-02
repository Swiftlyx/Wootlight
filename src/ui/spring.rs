//! Damped springs for every animated value, keyed by the element and property they belong to.

use std::{collections::HashMap, time::Instant};

use super::design::Spring;

const SETTLED: f32 = 0.002;
const MAX_STEP: f32 = 0.05;

/// Advances a damped spring of unit mass by `dt` with the exact solution, so any frame time is stable.
pub fn step(value: &mut f32, velocity: &mut f32, target: f32, spring: Spring, dt: f32) {
    let omega = spring.stiffness.sqrt();
    let zeta = spring.damping;
    let x0 = *value - target;
    let v0 = *velocity;
    if zeta < 1.0 {
        let omega_d = omega * (1.0 - zeta * zeta).sqrt();
        let decay = (-zeta * omega * dt).exp();
        let (s, c) = (omega_d * dt).sin_cos();
        *value = target + decay * (x0 * c + (v0 + zeta * omega * x0) / omega_d * s);
        *velocity = decay * (v0 * c - (zeta * omega * v0 + omega * omega * x0) / omega_d * s);
    } else {
        let decay = (-omega * dt).exp();
        let b = v0 + omega * x0;
        *value = target + (x0 + b * dt) * decay;
        *velocity = (v0 - omega * b * dt) * decay;
    }
}

struct State {
    value: f32,
    velocity: f32,
    last: Instant,
}

#[derive(Default)]
pub struct Animator {
    states: HashMap<(u64, u8), State>,
    moving: bool,
}

impl Animator {
    /// Current value of the spring for `key`, moving towards `target`. A new key starts at its target.
    pub fn get(&mut self, key: (u64, u8), target: f32, spring: Spring) -> f32 {
        let now = Instant::now();
        let state = self.states.entry(key).or_insert(State { value: target, velocity: 0.0, last: now });
        let dt = now.duration_since(state.last).as_secs_f32().min(MAX_STEP);
        state.last = now;
        if dt > 0.0 {
            step(&mut state.value, &mut state.velocity, target, spring, dt);
        }
        let scale = target.abs().max(1.0);
        if (state.value - target).abs() < SETTLED * scale && state.velocity.abs() < SETTLED * scale * 10.0 {
            state.value = target;
            state.velocity = 0.0;
        } else {
            self.moving = true;
        }
        state.value
    }

    /// Jumps a spring to a value, e.g. the scroll position when the page changes.
    pub fn set(&mut self, key: (u64, u8), value: f32) {
        self.states.insert(key, State { value, velocity: 0.0, last: Instant::now() });
    }

    /// True if any spring moved since the last call, so another frame is needed.
    pub fn take_moving(&mut self) -> bool {
        std::mem::take(&mut self.moving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::design::{SPRING_FAST_EFFECTS, SPRING_FAST_SPATIAL};

    fn settle(spring: Spring) -> (f32, f32) {
        let (mut value, mut velocity, mut peak) = (0.0f32, 0.0f32, 0.0f32);
        for _ in 0..200 {
            step(&mut value, &mut velocity, 1.0, spring, 1.0 / 120.0);
            peak = peak.max(value);
        }
        (value, peak)
    }

    #[test]
    fn spatial_springs_overshoot_effects_do_not() {
        let (end, peak) = settle(SPRING_FAST_SPATIAL);
        assert!((end - 1.0).abs() < 1e-3 && peak > 1.01);
        let (end, peak) = settle(SPRING_FAST_EFFECTS);
        assert!((end - 1.0).abs() < 1e-3 && peak <= 1.0 + 1e-4);
    }
}
