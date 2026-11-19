//! The drawn times and traits follow the model: sample means and spreads
//! against the closed forms of the model, or against the model's formulas
//! written out again here, independently of the code under test.

use std::collections::HashSet;
use std::f64::consts::PI;

use super::math::{Rnd, clamp};
use super::pauses::Reading;
use super::{Human, PauseKind, ReadingKind, ScrollStyle, Speed, Traits};

const N: usize = 200_000;

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, var.sqrt())
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn sample(n: usize, mut f: impl FnMut() -> f64) -> Vec<f64> {
    (0..n).map(|_| f()).collect()
}

#[track_caller]
fn assert_close(what: &str, actual: f64, expected: f64, rel: f64) {
    let err = (actual - expected).abs() / expected.abs().max(1e-12);
    assert!(
        err <= rel,
        "{what}: {actual} vs {expected} (off by {:.2}%, allowed {:.2}%)",
        err * 100.0,
        rel * 100.0
    );
}

#[track_caller]
fn assert_share(what: &str, hits: usize, n: usize, p: f64) {
    let share = hits as f64 / n as f64;
    // Four standard errors.
    let tol = 4.0 * (p * (1.0 - p) / n as f64).sqrt();
    assert!(
        (share - p).abs() <= tol,
        "{what}: {share:.4} vs {p:.4} (allowed ±{tol:.4})"
    );
}

/// The standard normal CDF (Abramowitz-Stegun 7.1.26, error < 1.5e-7).
fn phi(z: f64) -> f64 {
    let x = z.abs() / 2f64.sqrt();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-x * x).exp();
    if z >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// E[min(X, cap)] for X log-normal(mu, sigma).
fn capped_lognormal_mean(mu: f64, sigma: f64, cap: f64) -> f64 {
    let mean = (mu + sigma * sigma / 2.0).exp();
    let lc = cap.ln();
    mean * phi((lc - mu - sigma * sigma) / sigma) + cap * (1.0 - phi((lc - mu) / sigma))
}

/// A person with the given speed factor and otherwise drawn traits.
fn person(seed: u64, speed: f64) -> Human {
    let h = Human::with_seed(seed);
    let mut t = h.traits();
    t.speed_factor = speed;
    h.set_traits(t);
    h
}

#[test]
fn random_distributions_match_their_closed_forms() {
    let rnd = Rnd::new(Some(1));

    let (m, sd) = mean_sd(&sample(N, || rnd.uniform(2.0, 5.0)));
    assert_close("uniform mean", m, 3.5, 0.005);
    assert_close("uniform sd", sd, 3.0 / 12f64.sqrt(), 0.01);

    let (m, sd) = mean_sd(&sample(N, || rnd.gauss(10.0, 3.0)));
    assert_close("gauss mean", m, 10.0, 0.005);
    assert_close("gauss sd", sd, 3.0, 0.01);

    let (mu, s) = (4.5, 0.38);
    let (m, sd) = mean_sd(&sample(N, || rnd.lognorm(mu, s)));
    let mean = (mu + s * s / 2.0).exp();
    assert_close("lognormal mean", m, mean, 0.005);
    assert_close(
        "lognormal sd",
        sd,
        mean * ((s * s).exp() - 1.0).sqrt(),
        0.02,
    );

    let (m, sd) = mean_sd(&sample(N, || rnd.expo(1.0 / 150.0)));
    assert_close("exponential mean", m, 150.0, 0.01);
    assert_close("exponential sd", sd, 150.0, 0.02);

    let (a, b, c) = (0.85, 1.15, 1.0);
    let (m, sd) = mean_sd(&sample(N, || rnd.triangular(a, b, c)));
    assert_close("triangular mean", m, (a + b + c) / 3.0, 0.002);
    let var = (a * a + b * b + c * c - a * b - a * c - b * c) / 18.0;
    assert_close("triangular sd", sd, var.sqrt(), 0.01);

    // Log-logistic: median alpha, mean alpha * (pi/beta) / sin(pi/beta).
    let (alpha, beta) = (95.0, 3.8);
    let xs = sample(N, || rnd.log_logistic(alpha, beta));
    assert_close("log-logistic median", median(&xs), alpha, 0.01);
    let k = PI / beta;
    assert_close(
        "log-logistic mean",
        mean_sd(&xs).0,
        alpha * k / k.sin(),
        0.02,
    );

    // Ex-Gaussian: mean mu + tau, variance sigma² + tau² (the floor at 50
    // is far in the tail).
    let (m, sd) = mean_sd(&sample(N, || rnd.ex_gaussian(200.0, 50.0, 150.0)));
    assert_close("ex-gaussian mean", m, 350.0, 0.01);
    assert_close(
        "ex-gaussian sd",
        sd,
        (50f64.powi(2) + 150f64.powi(2)).sqrt(),
        0.02,
    );

    let ints = sample(N, || rnd.randint(3, 7) as f64);
    for v in 3..=7 {
        let hits = ints.iter().filter(|x| **x == v as f64).count();
        assert_share(&format!("randint {v}"), hits, N, 0.2);
    }
    let minus = (0..N).filter(|_| rnd.sign() < 0.0).count();
    assert_share("sign", minus, N, 0.5);

    let picks = sample(N, || {
        rnd.weighted(&[0.0, 1.0, 2.0, 3.0], &[30.0, 35.0, 15.0, 20.0])
    });
    for (v, p) in [(0.0, 0.30), (1.0, 0.35), (2.0, 0.15), (3.0, 0.20)] {
        let hits = picks.iter().filter(|x| **x == v).count();
        assert_share(&format!("weighted {v}"), hits, N, p);
    }
}

#[test]
fn traits_follow_the_model() {
    let rnd = Rnd::new(Some(2));
    let n = 100_000;
    let all: Vec<Traits> = (0..n).map(|_| Traits::draw(&rnd)).collect();
    let col = |f: &dyn Fn(&Traits) -> f64| all.iter().map(f).collect::<Vec<f64>>();
    let count = |f: &dyn Fn(&Traits) -> bool| all.iter().filter(|t| f(t)).count();

    // Speed: 88% uniform 0.75-1.25, 12% half 0.55-0.75 and half 1.25-1.6.
    let expected = 0.88 * 1.0 + 0.12 * 0.5 * (0.65 + 1.425);
    assert_close(
        "speed mean",
        mean_sd(&col(&|t| t.speed_factor)).0,
        expected,
        0.003,
    );
    let outside = count(&|t| !(0.75..1.25).contains(&t.speed_factor));
    assert_share("speed outliers", outside, n, 0.12);

    // Typing rhythm: log-normal(0, 0.35) clamped to 0.55-2.2 (median 1).
    let rhythm = col(&|t| t.typing_rhythm);
    assert_close("rhythm median", median(&rhythm), 1.0, 0.01);
    let low = count(&|t| t.typing_rhythm <= 0.55);
    assert_share("rhythm at the floor", low, n, phi(0.55f64.ln() / 0.35));
    let high = count(&|t| t.typing_rhythm >= 2.2);
    assert_share(
        "rhythm at the ceiling",
        high,
        n,
        1.0 - phi(2.2f64.ln() / 0.35),
    );

    for (what, f, mean) in [
        (
            "flight sigma offset",
            (|t: &Traits| t.flight_sigma_offset) as fn(&Traits) -> f64,
            0.0,
        ),
        ("dwell mu", |t| t.dwell_mu, 4.3),
        ("dwell sigma", |t| t.dwell_sigma, 0.35),
        ("tremor", |t| t.tremor_sigma, 0.55),
        ("warm-up factor", |t| t.warmup_factor, 1.3),
        ("warm-up keys", |t| t.warmup_chars as f64, 5.0),
        ("fatigue", |t| t.fatigue_rate, 0.0005),
        ("noise seed", |t| t.noise_seed, 500.0),
        // The hesitation is uniform 0.02-0.05 times the rhythm clamped to 0.8-1.6.
        (
            "hesitation / rhythm",
            |t| t.hesitation_chance / clamp(t.typing_rhythm, 0.8, 1.6),
            0.035,
        ),
    ] {
        let m = mean_sd(&col(&f)).0;
        if mean == 0.0 {
            assert!(m.abs() < 0.005, "{what}: {m}");
        } else {
            assert_close(what, m, mean, 0.006);
        }
    }
    assert!(all.iter().all(|t| (3..=7).contains(&t.warmup_chars)));

    // Tab: 55% use it 55-95% of the time, the others 0-10%.
    assert_close(
        "prob tab mean",
        mean_sd(&col(&|t| t.prob_tab)).0,
        0.55 * 0.75 + 0.45 * 0.05,
        0.01,
    );
    assert_share("tab users", count(&|t| t.prob_tab >= 0.55), n, 0.55);
    // Enter: 45% use it 60-95% of the time, the others 0-8%.
    assert_close(
        "prob enter mean",
        mean_sd(&col(&|t| t.prob_enter)).0,
        0.45 * 0.775 + 0.55 * 0.04,
        0.01,
    );
    assert_share("enter users", count(&|t| t.prob_enter >= 0.6), n, 0.45);
    assert_share(
        "clicks when focused",
        count(&|t| t.clicks_when_focused),
        n,
        0.35,
    );
    assert_share("reads texts", count(&|t| t.reads_texts), n, 0.75);
    for (style, p) in [
        (ScrollStyle::Continuous, 0.30),
        (ScrollStyle::Hiccups, 0.35),
        (ScrollStyle::Flick, 0.15),
        (ScrollStyle::Slow, 0.20),
    ] {
        assert_share(style.as_str(), count(&|t| t.scroll_style == style), n, p);
    }
}

#[test]
fn pauses_follow_the_model() {
    let h = person(3, 1.0);
    let ln = |mu: f64, s: f64| capped_lognormal_mean(mu, s, 15_000.0) / 1000.0;
    for (kind, mean) in [
        (PauseKind::PageLoad, ln(7.5, 0.4)),
        (PauseKind::AfterClick, 0.200 + 0.150),
        (PauseKind::AfterSubmit, ln(8.2, 0.5)),
        (PauseKind::AfterModal, 0.250 + 0.200),
        (PauseKind::Orientation, ln(7.0, 0.5)),
        (PauseKind::Micro, (0.15 + 0.5) / 2.0),
        (PauseKind::Polling, (0.25 + 0.6) / 2.0),
        (PauseKind::General, ln(6.8, 0.5)),
    ] {
        let xs = sample(N, || h.pause_s(kind));
        assert_close(&format!("{kind:?}"), mean_sd(&xs).0, mean, 0.012);
        assert!(xs.iter().all(|x| *x <= 15.0), "{kind:?} over 15 s");
    }
    // The person's speed scales every pause.
    let slow = person(4, 1.4);
    let m = mean_sd(&sample(N, || slow.pause_s(PauseKind::Micro))).0;
    assert_close("Micro at speed 1.4", m, 0.325 * 1.4, 0.01);
}

#[test]
fn readings_follow_the_model() {
    let speed = 1.2;
    let h = person(5, speed);
    // (kind, duration s, with the mouse, chance of thinking, chance of skipping)
    for (kind, (lo, hi), mouse, think, skip) in [
        (ReadingKind::BeforePassword, (2.0, 5.0), true, 0.5, 0.0),
        (ReadingKind::BeforeName, (0.4, 1.2), true, 0.0, 0.0),
        (ReadingKind::BetweenRelated, (0.15, 0.6), false, 0.0, 0.4),
        (ReadingKind::BeforeAddress, (2.5, 5.5), true, 0.6, 0.0),
        (ReadingKind::BeforeAction, (1.0, 3.5), true, 0.3, 0.0),
        (ReadingKind::ScanningPage, (1.5, 4.0), true, 0.0, 0.0),
        (ReadingKind::AfterError, (2.0, 4.0), true, 0.7, 0.0),
        (ReadingKind::CookieConsent, (0.8, 2.5), true, 0.05, 0.0),
        (ReadingKind::Default, (1.0, 3.0), true, 0.1, 0.0),
    ] {
        let n = 100_000;
        let plans: Vec<Reading> = (0..n).map(|_| h.reading_plan(kind)).collect();
        let glances: Vec<f64> = plans
            .iter()
            .filter_map(|p| match p {
                Reading::Glance(ms) => Some(*ms),
                _ => None,
            })
            .collect();
        let thinks = plans
            .iter()
            .filter(|p| matches!(p, Reading::Think(_)))
            .count();
        assert_share(&format!("{kind:?} skipped"), glances.len(), n, skip);
        assert_share(
            &format!("{kind:?} thinking"),
            thinks,
            n,
            (1.0 - skip) * think,
        );
        if !glances.is_empty() {
            assert_close(&format!("{kind:?} glance"), mean_sd(&glances).0, 80.0, 0.01);
        }
        // Thinking or looking: uniform duration × speed × triangular jitter (mean 1).
        let secs: Vec<f64> = plans
            .iter()
            .filter_map(|p| match p {
                Reading::Think(s) => Some(*s),
                Reading::Look { seconds, mouse: m } => {
                    assert_eq!(*m, mouse, "{kind:?}");
                    Some(*seconds)
                }
                Reading::Glance(_) => None,
            })
            .collect();
        assert_close(
            &format!("{kind:?} duration"),
            mean_sd(&secs).0,
            (lo + hi) / 2.0 * speed,
            0.01,
        );
    }
}

/// The movement time of the model, written out again.
fn reference_movement_ms(rnd: &Rnd, dist: f64, width: f64) -> f64 {
    let mut mt = if dist < 1.0 || width < 1.0 {
        rnd.uniform(50.0, 150.0)
    } else {
        let id_bits = (dist / width + 1.0).log2();
        let base = 70.0 + 150.0 * id_bits;
        (base + rnd.gauss(0.0, base * 0.15)).max(80.0)
    };
    mt = mt.max(rnd.uniform(180.0, 300.0));
    if dist / mt * 1000.0 > 1500.0 {
        mt = dist / 1500.0 * 1000.0;
    }
    mt
}

#[test]
fn movement_times_follow_fitts_law_with_a_floor_and_a_speed_cap() {
    let h = Human::with_seed(6);
    let reference = Rnd::new(Some(60));
    for (dist, width) in [
        (50.0, 50.0),
        (300.0, 40.0),
        (800.0, 20.0),
        (1500.0, 200.0),
        (120.0, 8.0),
    ] {
        let xs = sample(N, || h.movement_ms(dist, width));
        let ys = sample(N, || reference_movement_ms(&reference, dist, width));
        let ((mx, sx), (my, sy)) = (mean_sd(&xs), mean_sd(&ys));
        assert_close(&format!("{dist}/{width} mean"), mx, my, 0.005);
        assert_close(&format!("{dist}/{width} sd"), sx, sy, 0.03);
        // Never faster than 1500 px/s on average.
        assert!(xs.iter().all(|t| dist / t * 1000.0 <= 1500.0 + 1e-6));
    }
    // A long move to a small target: the plain Fitts time.
    let m = mean_sd(&sample(N, || h.movement_ms(800.0, 20.0))).0;
    assert_close("Fitts 800/20", m, 70.0 + 150.0 * 41f64.log2(), 0.01);
}

/// The time between keys of the model, written out again.
fn reference_flight_ms(rnd: &Rnd, t: &Traits, text: &[char], i: usize, ab: (f64, f64)) -> f64 {
    const LEFT: &str = "qwertasdfgzxcvb12345`~!@#$%";
    const RIGHT: &str = "yuiophjklnm67890-=[]\\;',./^&*(";
    let digraphs = [("th", 0.76), ("xq", 1.30), ("es", 0.77), ("ce", 1.15)];
    let (prev, ch) = (text[i - 1], text[i]);
    let mut fl = rnd.log_logistic(ab.0, ab.1);
    let pair: String = [prev.to_ascii_lowercase(), ch.to_ascii_lowercase()]
        .iter()
        .collect();
    fl *= digraphs
        .iter()
        .find(|(p, _)| *p == pair)
        .map_or(1.0, |d| d.1);
    let (a, b) = (prev.to_ascii_lowercase(), ch.to_ascii_lowercase());
    if (LEFT.contains(a) && RIGHT.contains(b)) || (RIGHT.contains(a) && LEFT.contains(b)) {
        fl *= rnd.uniform(0.73, 0.82);
    }
    if i < t.warmup_chars {
        fl *= t.warmup_factor;
    }
    fl *= 1.0 + i as f64 * t.fatigue_rate;
    if prev == ' ' && ch != ' ' {
        fl += rnd.uniform(25.0, 55.0);
    }
    fl = fl.clamp(25.0, 900.0);
    if rnd.random() < t.hesitation_chance {
        fl += rnd.uniform(180.0, 650.0);
    }
    let digit = |c: char| c.is_ascii_digit();
    if (digit(prev) && ch.is_alphabetic()) || (prev.is_alphabetic() && digit(ch)) {
        fl += rnd.uniform(90.0, 240.0);
    } else if ch == '@' || prev == '@' {
        fl += rnd.uniform(120.0, 300.0);
    } else if ".,*/-_+#%ºª".contains(ch) || ".,*/-_+#%ºª".contains(prev) {
        fl += rnd.uniform(70.0, 200.0);
    } else if digit(prev) && digit(ch) {
        let back = text[..i].iter().rev().take_while(|c| digit(**c)).count();
        let ahead = text[i..].iter().take_while(|c| digit(**c)).count();
        if back + ahead >= 8 && back % 4 == 0 {
            fl += rnd.uniform(110.0, 280.0);
        }
    }
    fl
}

#[test]
fn times_between_keys_follow_the_model() {
    let h = Human::with_seed(7);
    let mut t = h.traits();
    t.typing_rhythm = 1.3;
    t.flight_sigma_offset = -0.2;
    t.warmup_chars = 4;
    t.warmup_factor = 1.3;
    t.fatigue_rate = 0.0006;
    t.hesitation_chance = 0.04;
    h.set_traits(t.clone());
    let reference = Rnd::new(Some(70));
    // alpha 95 × rhythm, beta 3.8 + offset (at least 2).
    let ((alpha, beta), _) = h.rhythm(&t, Speed::Normal);
    assert_close("alpha", alpha, 95.0 * 1.3, 1e-9);
    assert_close("beta", beta, 3.6, 1e-9);
    let ((_, beta_fast), _) = h.rhythm(&t, Speed::Fast);
    assert_close("fast beta", beta_fast, 4.3, 1e-9);
    let error_rates = sample(N, || h.rhythm(&t, Speed::Normal).1);
    assert_close("error rate", mean_sd(&error_rates).0, 0.0275 * 1.3, 0.01);

    for (text, i) in [
        ("xq", 1),         // a slow pair, same hand
        ("lapis", 3),      // p→i: same hand
        ("map", 2),        // m→a: opposite hands
        ("the cat", 1),    // a fast pair, warm-up
        ("the cat", 4),    // after a space
        ("ana88", 3),      // letter → digit
        ("ana@x", 4),      // after @
        ("a.b", 1),        // punctuation
        ("1234567890", 4), // a long number: a pause every 4 digits
        ("1234567890", 5), // ... but not inside a block
        ("1234", 2),       // a short number: no pause
    ] {
        let chars: Vec<char> = text.chars().collect();
        let xs = sample(N, || h.flight_ms(&t, &chars, i, (alpha, beta), None));
        let ys = sample(N, || {
            reference_flight_ms(&reference, &t, &chars, i, (alpha, beta))
        });
        assert_close(
            &format!("{text}[{i}] mean"),
            mean_sd(&xs).0,
            mean_sd(&ys).0,
            0.012,
        );
        assert_close(
            &format!("{text}[{i}] median"),
            median(&xs),
            median(&ys),
            0.015,
        );
    }

    // With blocks: only the block starts get the longer pause.
    // (One repeated digit, so the hands and pairs are the same everywhere.)
    let date: Vec<char> = "11111111".chars().collect();
    let starts: HashSet<usize> = [2, 4].into_iter().collect();
    let at = mean_sd(&sample(N, || {
        h.flight_ms(&t, &date, 4, (alpha, beta), Some(&starts))
    }))
    .0;
    let inside = mean_sd(&sample(N, || {
        h.flight_ms(&t, &date, 5, (alpha, beta), Some(&starts))
    }))
    .0;
    // 220-600 ms more on average (the fatigue of one key apart is negligible).
    assert_close("block pause", at - inside, 410.0, 0.03);
}
