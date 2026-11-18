use std::f64::consts::TAU;

fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let at = |p1: f64, p2: f64, s: f64| {
        3.0 * (1.0 - s).powi(2) * s * p1 + 3.0 * (1.0 - s) * s.powi(2) * p2 + s.powi(3)
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if at(x1, x2, mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(y1, y2, (lo + hi) / 2.0)
}

pub(super) fn ease_in_out(x: f64) -> f64 {
    cubic_bezier(0.42, 0.0, 0.58, 1.0, x)
}

pub(super) fn ease_out(x: f64) -> f64 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, x)
}

pub(super) fn morph_curve(x: f64) -> f64 {
    cubic_bezier(0.45, 0.0, 0.2, 1.0, x)
}

pub(super) fn wave(t: u64, period: u64) -> f64 {
    let phase = (t % period) as f64 / period as f64;
    0.5 - 0.5 * (TAU * phase).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r3(v: f64) -> f64 {
        (v * 1000.0).round() / 1000.0
    }

    #[test]
    fn the_curves_reproduce_the_spec_tables() {
        let rise: Vec<f64> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .map(|x| r3(ease_in_out(x)))
            .to_vec();
        assert_eq!(rise, [0.0, 0.129, 0.5, 0.871, 1.0]);
        let decay: Vec<f64> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .map(|x| r3(1.0 - ease_out(x)))
            .to_vec();
        assert_eq!(decay, [1.0, 0.622, 0.315, 0.093, 0.0]);
        let steps: Vec<f64> = [1.0, 2.0, 3.0].map(|i| r3(morph_curve(i / 3.0))).to_vec();
        assert_eq!(steps, [0.401, 0.919, 1.0]);
    }

    #[test]
    fn the_wave_reproduces_the_working_pulse() {
        let pulse: Vec<f64> = (0..=10).map(|i| r3(wave(i * 100, 2000))).collect();
        assert_eq!(
            pulse,
            [
                0.0, 0.024, 0.095, 0.206, 0.345, 0.5, 0.655, 0.794, 0.905, 0.976, 1.0
            ]
        );
        assert_eq!(r3(wave(2000, 4000)), 1.0);
        assert_eq!(r3(wave(4000, 4000)), 0.0);
    }
}
