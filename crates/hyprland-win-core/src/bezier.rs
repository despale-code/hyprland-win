//! Parametric Cubic Bezier Curve Solver for Hyprland Animations (`bezier = name, x0, y0, x1, y1`).
//!
//! Solves $x(t) = u$ via Newton-Raphson iteration with bisection fallback and evaluates $y(t)$,
//! supporting overshooting curves (e.g. `bezier = overshot, 0.13, 0.99, 0.29, 1.1`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl Default for CubicBezier {
    fn default() -> Self {
        // Hyprland's default `myBezier` curve: 0.05, 0.9, 0.1, 1.05
        Self {
            x1: 0.05,
            y1: 0.90,
            x2: 0.10,
            y2: 1.05,
        }
    }
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    #[inline]
    fn sample_curve_x(&self, t: f32) -> f32 {
        let inv = 1.0 - t;
        3.0 * inv * inv * t * self.x1 + 3.0 * inv * t * t * self.x2 + t * t * t
    }

    #[inline]
    fn sample_curve_y(&self, t: f32) -> f32 {
        let inv = 1.0 - t;
        3.0 * inv * inv * t * self.y1 + 3.0 * inv * t * t * self.y2 + t * t * t
    }

    #[inline]
    fn sample_derivative_x(&self, t: f32) -> f32 {
        let inv = 1.0 - t;
        3.0 * inv * inv * self.x1
            + 6.0 * inv * t * (self.x2 - self.x1)
            + 3.0 * t * t * (1.0 - self.x2)
    }

    /// Evaluate the bezier Y output for a normalized time progress `progress` in `[0.0, 1.0]`.
    pub fn evaluate(&self, progress: f32) -> f32 {
        if progress <= 0.0 {
            return 0.0;
        }
        if progress >= 1.0 {
            return 1.0;
        }

        // Newton-Raphson solver for t given x = progress
        let mut t = progress;
        for _ in 0..8 {
            let x_err = self.sample_curve_x(t) - progress;
            if x_err.abs() < 1e-5 {
                return self.sample_curve_y(t);
            }
            let dx = self.sample_derivative_x(t);
            if dx.abs() < 1e-5 {
                break;
            }
            t = (t - x_err / dx).clamp(0.0, 1.0);
        }

        // Bisection fallback
        let mut low = 0.0_f32;
        let mut high = 1.0_f32;
        t = progress;
        for _ in 0..12 {
            let x = self.sample_curve_x(t);
            if (x - progress).abs() < 1e-5 {
                break;
            }
            if progress > x {
                low = t;
            } else {
                high = t;
            }
            t = 0.5 * (low + high);
        }

        self.sample_curve_y(t)
    }
}

/// Floating-point rectangle for smooth sub-pixel animated window geometry.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn lerp(from: Self, to: Self, factor: f32) -> Self {
        Self {
            x: from.x + (to.x - from.x) * factor,
            y: from.y + (to.y - from.y) * factor,
            w: from.w + (to.w - from.w) * factor,
            h: from.h + (to.h - from.h) * factor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_overshoot() {
        let curve = CubicBezier::new(0.13, 0.99, 0.29, 1.1);
        assert_eq!(curve.evaluate(0.0), 0.0);
        assert_eq!(curve.evaluate(1.0), 1.0);
        let mid = curve.evaluate(0.6);
        assert!(mid > 0.9, "expected overshot bezier at t=0.6 to exceed 0.9, got {mid}");
    }
}
