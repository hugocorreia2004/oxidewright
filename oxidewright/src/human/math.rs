//! Random distributions, Perlin noise and the mouse path, all pure (no
//! browser), so they can be tested on their own.

use std::f64::consts::PI;
use std::sync::Mutex;

/// Thread-safe random source with the distributions the simulation uses.
pub(crate) struct Rnd {
    rng: Mutex<fastrand::Rng>,
}

impl Rnd {
    pub(crate) fn new(seed: Option<u64>) -> Self {
        Self {
            rng: Mutex::new(seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed)),
        }
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn random(&self) -> f64 {
        self.rng.lock().unwrap().f64()
    }

    /// Uniform in `[a, b)`.
    pub(crate) fn uniform(&self, a: f64, b: f64) -> f64 {
        a + self.random() * (b - a)
    }

    /// Integer in `[a, b]`.
    pub(crate) fn randint(&self, a: i64, b: i64) -> i64 {
        if b <= a {
            return a;
        }
        self.rng.lock().unwrap().i64(a..=b)
    }

    /// `-1.0` or `1.0`.
    pub(crate) fn sign(&self) -> f64 {
        if self.random() < 0.5 { -1.0 } else { 1.0 }
    }

    pub(crate) fn index(&self, len: usize) -> usize {
        if len <= 1 {
            0
        } else {
            self.rng.lock().unwrap().usize(0..len)
        }
    }

    pub(crate) fn choice<'a, T>(&self, xs: &'a [T]) -> &'a T {
        &xs[self.index(xs.len())]
    }

    /// One of `xs`, weighted by `weights`.
    pub(crate) fn weighted<T: Copy>(&self, xs: &[T], weights: &[f64]) -> T {
        let total: f64 = weights.iter().sum();
        let mut r = self.random() * total;
        for (x, w) in xs.iter().zip(weights) {
            r -= w;
            if r < 0.0 {
                return *x;
            }
        }
        xs[xs.len() - 1]
    }

    /// Normal distribution (Box-Muller).
    pub(crate) fn gauss(&self, mu: f64, sigma: f64) -> f64 {
        let mut u = 0.0;
        let mut v = 0.0;
        while u == 0.0 {
            u = self.random();
        }
        while v == 0.0 {
            v = self.random();
        }
        mu + sigma * (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
    }

    /// Log-normal: `exp(gauss(mu, sigma))`.
    pub(crate) fn lognorm(&self, mu: f64, sigma: f64) -> f64 {
        self.gauss(mu, sigma).exp()
    }

    /// Exponential with mean `1 / lambda`.
    pub(crate) fn expo(&self, lambda: f64) -> f64 {
        -(1.0 - self.random()).ln() / lambda
    }

    pub(crate) fn triangular(&self, low: f64, high: f64, mode: f64) -> f64 {
        let u = self.random();
        let c = (mode - low) / (high - low);
        if u < c {
            low + (u * (high - low) * (mode - low)).sqrt()
        } else {
            high - ((1.0 - u) * (high - low) * (high - mode)).sqrt()
        }
    }

    /// Ex-Gaussian in ms, at least 50.
    pub(crate) fn ex_gaussian(&self, mu: f64, sigma: f64, tau: f64) -> f64 {
        (self.gauss(mu, sigma) + self.expo(1.0 / tau)).max(50.0)
    }

    /// Log-logistic(alpha, beta).
    pub(crate) fn log_logistic(&self, alpha: f64, beta: f64) -> f64 {
        let mut u = self.random();
        while u <= 0.0 || u >= 1.0 {
            u = self.random();
        }
        alpha * (u / (1.0 - u)).powf(1.0 / beta)
    }

    /// A shuffled permutation of `0..256`, doubled, for the Perlin noise.
    pub(crate) fn permutation(&self) -> Vec<usize> {
        let mut p: Vec<usize> = (0..256).collect();
        for i in (1..p.len()).rev() {
            let j = self.index(i + 1);
            p.swap(i, j);
        }
        let mut doubled = p.clone();
        doubled.extend(p);
        doubled
    }
}

pub(crate) fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// Keeps a point inside the usual viewport area.
pub(crate) fn clamp_viewport(x: f64, y: f64) -> (f64, f64) {
    (clamp(x, 50.0, 1800.0), clamp(y, 50.0, 1000.0))
}

/// The maximum mean mouse speed, px/s.
pub(crate) const MAX_SPEED: f64 = 1500.0;

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

fn grad1d(h: usize, x: f64) -> f64 {
    if h & 1 == 0 { x } else { -x }
}

/// 1D Perlin noise in about `[-0.5, 0.5]`.
pub(crate) fn pnoise1(perm: &[usize], x: f64, octaves: u32, persistence: f64) -> f64 {
    let mut total = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = 1.0;
    let mut max = 0.0;
    for _ in 0..octaves {
        let xf_floor = (x * frequency).floor();
        let xi = (xf_floor as i64 & 255) as usize;
        let xf = x * frequency - xf_floor;
        let u = fade(xf);
        let a = grad1d(perm[xi], xf);
        let b = grad1d(perm[xi + 1], xf - 1.0);
        total += lerp(a, b, u) * amplitude;
        max += amplitude;
        amplitude *= persistence;
        frequency *= 2.0;
    }
    if max > 0.0 { total / max } else { 0.0 }
}

/// Fitts' law: the time (ms) to move `distance` px onto a target `width` px wide.
pub(crate) fn fitts_time(rnd: &Rnd, distance: f64, width: f64) -> f64 {
    if distance < 1.0 || width < 1.0 {
        return rnd.uniform(50.0, 150.0);
    }
    let id_bits = (distance / width + 1.0).log2();
    let mt = 70.0 + 150.0 * id_bits;
    (mt + rnd.gauss(0.0, mt * 0.15)).max(80.0)
}

/// Asymmetric time remap: the speed peaks at 40% of the movement.
fn asymmetric_remap(t: f64) -> f64 {
    let peak = 0.4;
    if t <= peak {
        0.5 * (t / peak).powf(1.6)
    } else {
        0.5 + 0.5 * ((t - peak) / (1.0 - peak)).powf(0.7)
    }
}

/// The classic position profile.
pub(crate) fn classic_profile(t: f64) -> f64 {
    fade(asymmetric_remap(t))
}

/// The smooth position profile: the CDF of a Beta(2.2, 2.8), so the speed is a
/// smooth bell peaking at 40% of the movement at ~1.7x the mean speed, like a
/// hand. Tabulated once and interpolated.
pub(crate) fn smooth_profile(t: f64) -> f64 {
    static TABLE: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let n = 400;
        let (a, b) = (2.2_f64, 2.8_f64);
        let mut acc = vec![0.0];
        let mut sum = 0.0;
        for i in 1..=n {
            let t = (i as f64 - 0.5) / n as f64;
            sum += t.powf(a - 1.0) * (1.0 - t).powf(b - 1.0);
            acc.push(sum);
        }
        acc.into_iter().map(|v| v / sum).collect()
    });
    let n = table.len() - 1;
    let x = clamp(t, 0.0, 1.0) * n as f64;
    let i = x.floor() as usize;
    if i >= n {
        return 1.0;
    }
    table[i] + (table[i + 1] - table[i]) * (x - i as f64)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

/// How a path is built.
pub(crate) struct PathStyle<'a> {
    /// Rounds each coordinate (0.1 px, or the physical pixel grid).
    pub round: &'a (dyn Fn(f64) -> f64 + Sync),
    /// Position (0-1) at each instant (0-1); `None` is the classic profile.
    pub profile: Option<fn(f64) -> f64>,
    /// How far the Perlin noise advances per point.
    pub noise_step: f64,
}

/// The mouse path from `(sx, sy)` to `(ex, ey)`: an arc, Perlin tremor and,
/// with the classic profile, a little white jitter. `n + 1` points, the last
/// one exactly on the target.
///
/// With a profile (the grid mode) the tremor is the one of a hand, slow and
/// smooth, and there is no point-to-point jitter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_path(
    rnd: &Rnd,
    perm: &[usize],
    (sx, sy): (f64, f64),
    (ex, ey): (f64, f64),
    n: usize,
    tremor: f64,
    seed: f64,
    style: &PathStyle<'_>,
) -> Vec<Point> {
    let (dx, dy) = (ex - sx, ey - sy);
    let dist = dx.hypot(dy);
    let (px, py) = if dist > 0.0 {
        (-dy / dist, dx / dist)
    } else {
        (0.0, 0.0)
    };
    let arc_amp = dist * rnd.uniform(0.02, 0.08) * rnd.sign();
    let mut points = Vec::with_capacity(n + 1);
    for i in 0..n {
        let t = i as f64 / (n.max(2) - 1) as f64;
        let pos = style.profile.map_or_else(|| classic_profile(t), |p| p(t));
        let arc = arc_amp * (PI * t).sin();
        let bx = sx + dx * pos + px * arc;
        let by = sy + dy * pos + py * arc;
        let tp = seed + i as f64 * style.noise_step;
        let octaves = if style.profile.is_some() { 2 } else { 3 };
        let tx = pnoise1(perm, tp, octaves, 0.5) * tremor;
        let ty = pnoise1(perm, tp + 100.0, octaves, 0.5) * tremor;
        let (jx, jy) = if style.profile.is_some() {
            (0.0, 0.0)
        } else {
            (rnd.gauss(0.0, tremor * 0.3), rnd.gauss(0.0, tremor * 0.3))
        };
        points.push(Point {
            x: (style.round)(bx + tx + jx),
            y: (style.round)(by + ty + jy),
        });
    }
    points.push(Point {
        x: (style.round)(ex),
        y: (style.round)(ey),
    });
    points
}

/// Rounds to 0.1 px.
pub(crate) fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Whether to overshoot the target (more likely on far, small targets).
pub(crate) fn should_overshoot(rnd: &Rnd, dist: f64, target_width: f64) -> bool {
    if dist < 100.0 {
        return false;
    }
    let id_bits = (dist / target_width.max(10.0) + 1.0).log2();
    rnd.random() < clamp(0.025 * id_bits, 0.03, 0.18)
}

/// A point a little past the target.
pub(crate) fn overshoot_point(rnd: &Rnd, ex: f64, ey: f64, dist: f64) -> (f64, f64) {
    let d = rnd.uniform(0.05, 0.15) * dist.min(200.0);
    let a = rnd.uniform(0.0, 2.0 * PI);
    (ex + a.cos() * d, ey + a.sin() * d)
}

const LEFT_HAND: &str = "qwertasdfgzxcvb12345`~!@#$%";
const RIGHT_HAND: &str = "yuiophjklnm67890-=[]\\;',./^&*(";

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

pub(crate) fn left_hand(c: char) -> bool {
    LEFT_HAND.contains(lower(c))
}

/// Keys typed with opposite hands follow each other faster.
pub(crate) fn hand_multiplier(rnd: &Rnd, a: char, b: char) -> f64 {
    let (la, ra) = (LEFT_HAND.contains(lower(a)), RIGHT_HAND.contains(lower(a)));
    let (lb, rb) = (LEFT_HAND.contains(lower(b)), RIGHT_HAND.contains(lower(b)));
    if (la && rb) || (ra && lb) {
        rnd.uniform(0.73, 0.82)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_profile_is_monotonic_from_0_to_1() {
        assert_eq!(smooth_profile(0.0), 0.0);
        assert_eq!(smooth_profile(1.0), 1.0);
        let mut last = 0.0;
        for i in 0..=100 {
            let v = smooth_profile(i as f64 / 100.0);
            assert!(v >= last);
            last = v;
        }
        // The speed peaks around 40%: the position there is under one half.
        assert!(smooth_profile(0.4) < 0.5 && smooth_profile(0.5) > 0.5);
    }

    #[test]
    fn path_starts_near_the_start_and_ends_on_the_target() {
        let rnd = Rnd::new(Some(7));
        let perm = rnd.permutation();
        let style = PathStyle {
            round: &round1,
            profile: None,
            noise_step: 0.15,
        };
        let path = build_path(
            &rnd,
            &perm,
            (10.0, 10.0),
            (400.0, 300.0),
            40,
            0.5,
            3.0,
            &style,
        );
        assert_eq!(path.len(), 41);
        assert!((path[0].x - 10.0).abs() < 3.0 && (path[0].y - 10.0).abs() < 3.0);
        assert_eq!(*path.last().unwrap(), Point { x: 400.0, y: 300.0 });
    }

    #[test]
    fn distributions_stay_in_range() {
        let rnd = Rnd::new(Some(1));
        for _ in 0..1000 {
            let v = rnd.uniform(2.0, 3.0);
            assert!((2.0..3.0).contains(&v));
            let t = rnd.triangular(0.85, 1.15, 1.0);
            assert!((0.85..=1.15).contains(&t));
            assert!(rnd.ex_gaussian(200.0, 50.0, 150.0) >= 50.0);
            assert!(rnd.log_logistic(95.0, 3.8) > 0.0);
            let i = rnd.randint(3, 7);
            assert!((3..=7).contains(&i));
        }
    }
}
