//! Software fan curves for the Balanced and Eco modes.
//!
//! The firmware has no fan-table support on this model (`GetGamingFanTable` is unsupported), so
//! the curve runs here: every tick the temperature is smoothed, mapped through the curve and the
//! result is written as a custom fan speed. Fans stay off below the curve's first point.
//!
//! `FanCurve` is the user-editable shape (validated so it is always safe); `FanTrack` is the
//! running state of one fan group. Percentages are real duty cycles; the controller converts
//! them to firmware values when it writes them.

use crate::protocol::round_to_u8;
use std::fmt;

/// Smoothing factor per tick (2 s ticks: about an 8 s average). Short spikes from starting an
/// app barely move the smoothed temperature, so the fans do not spin up for them.
pub const EMA_ALPHA: f32 = 0.25;
/// At or above this the raw temperature is used directly: no lag when it really gets hot.
pub const FAST_AT: f32 = 90.0;
/// Fans stop only once the smoothed temperature is this far below the start temperature.
pub const STOP_HYSTERESIS: f32 = 3.0;
/// Small decreases are ignored ...
pub const DOWN_DEADBAND: u8 = 4;
/// ... larger ones happen in steps of at most this much per tick.
pub const DOWN_STEP: u8 = 10;

/// Temperatures a curve point may use (°C).
const POINT_TEMPS: std::ops::RangeInclusive<u8> = 40..=95;
/// Most points a curve may have.
const MAX_POINTS: usize = 8;

/// Points `(temperature °C, fan duty %)`, temperatures strictly rising, ending at 100 %.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanCurve {
    points: Vec<(u8, u8)>,
}

impl FanCurve {
    /// Full performance, fans as quiet as possible. In real duty; these sound exactly like the
    /// earlier 25/35/55/80/100 %, which were firmware values (see `protocol::fan_duty_of`).
    #[must_use]
    pub fn balanced_default() -> FanCurve {
        FanCurve { points: vec![(60, 10), (70, 16), (80, 32), (87, 62), (92, 100)] }
    }

    /// Power-limited Eco mode: less heat, so the fans start later and stay lower. Earlier
    /// firmware values 20/35/60/100 %.
    #[must_use]
    pub fn eco_default() -> FanCurve {
        FanCurve { points: vec![(65, 7), (75, 16), (85, 35), (90, 100)] }
    }

    /// The same curve with every percentage passed through `f` (converting old settings).
    /// `None` if the result breaks the curve rules.
    pub fn map_pct(&self, f: impl Fn(u8) -> u8) -> Option<FanCurve> {
        let text: Vec<String> = self.points.iter().map(|&(t, p)| format!("{t}:{}", f(p))).collect();
        FanCurve::parse(&text.join(","))
    }

    /// Parse `"60:25,70:35,...,92:100"`. Rejected (None) unless: 1..=8 points, temperatures
    /// 40..=95 °C strictly rising, percentages 0..=100 never falling, last point 100 %.
    /// The last rule keeps every curve safe: full fan speed at 95 °C at the latest.
    #[must_use]
    pub fn parse(text: &str) -> Option<FanCurve> {
        let mut points = Vec::new();
        for part in text.split(',') {
            let (temp, pct) = part.trim().split_once(':')?;
            let temp: u8 = temp.trim().parse().ok()?;
            let pct: u8 = pct.trim().parse().ok()?;
            if !POINT_TEMPS.contains(&temp) || pct > 100 {
                return None;
            }
            if let Some(&(last_temp, last_pct)) = points.last() {
                if temp <= last_temp || pct < last_pct {
                    return None;
                }
            }
            points.push((temp, pct));
        }
        if points.is_empty() || points.len() > MAX_POINTS || points.last()?.1 != 100 {
            return None;
        }
        Some(FanCurve { points })
    }

    /// The `(temperature, duty)` points, coolest first.
    #[must_use]
    pub fn points(&self) -> &[(u8, u8)] {
        &self.points
    }

    /// Temperature at which the fans start.
    #[must_use]
    pub fn start(&self) -> u8 {
        self.points[0].0
    }

    /// Fan percentage for a temperature: 0 below the first point, linear between points.
    ///
    /// # Panics
    ///
    /// Never: a `FanCurve` always has at least one point (`parse` rejects empty curves).
    #[must_use]
    pub fn pct_at(&self, t: f32) -> u8 {
        let first = self.points[0];
        if t < f32::from(first.0) {
            return 0;
        }
        for segment in self.points.windows(2) {
            let ((t0, p0), (t1, p1)) = (segment[0], segment[1]);
            if t < f32::from(t1) {
                let frac = (t - f32::from(t0)) / f32::from(t1 - t0);
                return round_to_u8(f32::from(p0) + frac * (f32::from(p1) - f32::from(p0))).min(100);
            }
        }
        self.points.last().unwrap().1
    }

    /// Lowest non-zero speed: the duty of the first point.
    fn min_pct(&self) -> u8 {
        self.points[0].1
    }
}

impl fmt::Display for FanCurve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (t, p)) in self.points.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{t}:{p}")?;
        }
        Ok(())
    }
}

/// Curve state of one fan group. The default has no history: its first `step` jumps straight
/// to the curve's value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FanTrack {
    /// Smoothed temperature (exponential moving average), `None` before the first reading.
    ema: Option<f32>,
    /// Fan duty currently commanded.
    pct: u8,
}

impl FanTrack {
    /// Fan duty currently commanded (%).
    #[must_use]
    pub fn pct(&self) -> u8 {
        self.pct
    }

    /// Smoothed temperature the speed is based on.
    #[must_use]
    pub fn temp(&self) -> Option<f32> {
        self.ema
    }

    /// Feed one raw temperature reading; returns the new fan percentage. Speeds go up at once
    /// but come down gradually (`DOWN_DEADBAND`, `DOWN_STEP`, `STOP_HYSTERESIS`).
    pub fn step(&mut self, curve: &FanCurve, raw: f32) -> u8 {
        let mut ema = match self.ema {
            None => raw,
            Some(e) => e + EMA_ALPHA * (raw - e),
        };
        if raw >= FAST_AT {
            ema = ema.max(raw);
        }
        self.ema = Some(ema);
        let target = curve.pct_at(ema);
        let cur = self.pct;
        self.pct = if target >= cur {
            target
        } else if target == 0 {
            // Below the start temperature: keep turning slowly until clearly cooler, then stop.
            // Stopping happens from the curve's lowest speed, so after a fast cool-down the fans
            // still step down first instead of going straight from e.g. 40 % to off.
            let min = curve.min_pct();
            if ema < f32::from(curve.start()) - STOP_HYSTERESIS && cur <= min {
                0
            } else {
                cur.saturating_sub(DOWN_STEP).max(min)
            }
        } else if cur - target <= DOWN_DEADBAND {
            cur
        } else {
            cur.saturating_sub(DOWN_STEP).max(target)
        };
        self.pct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Behaviour tests use fixed curves so they do not depend on the defaults.
    fn old_balanced() -> FanCurve {
        FanCurve::parse("60:25,70:35,80:55,87:80,92:100").unwrap()
    }

    fn old_eco() -> FanCurve {
        FanCurve::parse("65:20,75:35,85:60,90:100").unwrap()
    }

    #[test]
    fn interpolation() {
        let c = old_balanced();
        assert_eq!(c.pct_at(30.0), 0);
        assert_eq!(c.pct_at(59.9), 0);
        assert_eq!(c.pct_at(60.0), 25);
        assert_eq!(c.pct_at(65.0), 30);
        assert_eq!(c.pct_at(75.0), 45);
        assert_eq!(c.pct_at(92.0), 100);
        assert_eq!(c.pct_at(110.0), 100);
        assert_eq!(c.start(), 60);
    }

    #[test]
    fn parse_and_format() {
        let c = FanCurve::parse(" 55:20, 70:40 ,90:100").unwrap();
        assert_eq!(c.points(), &[(55, 20), (70, 40), (90, 100)]);
        assert_eq!(FanCurve::parse(&c.to_string()), Some(c));
        assert_eq!(FanCurve::parse(&FanCurve::balanced_default().to_string()), Some(FanCurve::balanced_default()));
        assert_eq!(FanCurve::parse("90:100"), Some(FanCurve { points: vec![(90, 100)] }));
    }

    #[test]
    fn defaults_sound_like_the_old_firmware_values() {
        let to_duty = |p| crate::protocol::fan_duty_of(0, p);
        assert_eq!(old_balanced().map_pct(to_duty), Some(FanCurve::balanced_default()));
        assert_eq!(old_eco().map_pct(to_duty), Some(FanCurve::eco_default()));
        // A conversion that breaks the rules (never reaching 100 %) is refused.
        assert_eq!(old_balanced().map_pct(|p| p.min(90)), None);
    }

    #[test]
    fn unsafe_or_malformed_curves_rejected() {
        for bad in [
            "",
            "60:25,70:35",        // never reaches 100 %
            "60:25,99:100",       // last point too hot
            "30:25,90:100",       // first point too cold to be meaningful
            "70:25,60:100",       // temperatures not rising
            "60:50,70:40,90:100", // percentage falling
            "60:25,70:101",       // over 100 %
            "60-25,90:100",       // syntax
            "60:25,,90:100",
            "a:b",
            "40:1,45:2,50:3,55:4,60:5,65:6,70:7,75:8,90:100", // too many points
        ] {
            assert_eq!(FanCurve::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn short_spike_does_not_start_the_fans() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 50.0), 0);
        // App start: CPU jumps to 80 °C for one tick (2 s), then back.
        assert_eq!(t.step(&c, 80.0), 0, "smoothed temp {:?}", t.temp());
        assert_eq!(t.step(&c, 52.0), 0);
        assert_eq!(t.step(&c, 51.0), 0);
    }

    #[test]
    fn sustained_load_ramps_up() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        t.step(&c, 50.0);
        let mut last = 0;
        for _ in 0..20 {
            last = t.step(&c, 80.0);
        }
        assert!((50..=55).contains(&last), "{last}");
    }

    #[test]
    fn very_hot_reacts_immediately() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        t.step(&c, 50.0);
        assert_eq!(t.step(&c, 92.0), 100);
    }

    #[test]
    fn ramp_down_is_gradual_with_deadband_and_stop_hysteresis() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 80.0), 55);
        // Slightly cooler: within the deadband, no change.
        assert_eq!(t.step(&c, 78.0), 55);
        // Much cooler: steps of at most 10 %.
        let mut seen = vec![];
        for _ in 0..30 {
            seen.push(t.step(&c, 45.0));
        }
        // Each step is at most 10 %, except stopping from the curve's minimum (25 %) to off.
        let min = c.points()[0].1;
        assert!(seen.windows(2).all(|w| w[0] - w[1] <= DOWN_STEP || (w[0] <= min && w[1] == 0)), "{seen:?}");
        assert_eq!(seen[..4], [45, 35, 25, 0]);
        assert_eq!(*seen.last().unwrap(), 0);
        // Near the start temperature the fans do not flap on and off.
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 60.0), 25);
        for _ in 0..10 {
            assert_eq!(t.step(&c, 58.5), 25, "stays at the minimum just below the start");
        }
        for _ in 0..10 {
            t.step(&c, 55.0);
        }
        assert_eq!(t.pct(), 0);
    }

    #[test]
    fn first_reading_starts_at_target() {
        let c = old_eco();
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 77.0), 40);
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 59.0), 0);
    }

    fn curve(text: &str) -> FanCurve {
        FanCurve::parse(text).unwrap_or_else(|| panic!("{text}"))
    }

    /// Speeds of a fresh track fed `raws`.
    fn run(c: &FanCurve, raws: &[f32]) -> Vec<u8> {
        let mut t = FanTrack::default();
        raws.iter().map(|&r| t.step(c, r)).collect()
    }

    fn ema_is(t: &FanTrack, want: f32) -> bool {
        t.temp().is_some_and(|e| (e - want).abs() < 1e-4)
    }

    #[test]
    fn temperature_limits_are_inclusive() {
        assert_eq!(curve("40:100").start(), 40);
        assert_eq!(curve("95:100").start(), 95);
        assert_eq!(curve("40:0,95:100").points(), &[(40, 0), (95, 100)]);
        for bad in ["39:100", "96:100", "39:0,60:100", "60:0,96:100", "0:100", "255:100"] {
            assert_eq!(FanCurve::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn percentage_limits() {
        assert_eq!(curve("60:0,90:100").points()[0], (60, 0));
        for bad in ["60:0,90:101", "60:101,90:100", "60:50,90:99", "90:99", "90:0", "60:100,90:99"] {
            assert_eq!(FanCurve::parse(bad), None, "{bad}");
        }
        assert_eq!(curve("90:100").points(), &[(90, 100)]);
    }

    #[test]
    fn eight_points_allowed_nine_rejected() {
        let eight = "40:1,45:2,50:3,55:4,60:5,65:6,70:7,95:100";
        assert_eq!(curve(eight).points().len(), 8);
        assert_eq!(FanCurve::parse(&format!("{eight},")), None);
        assert_eq!(FanCurve::parse("40:1,45:2,50:3,55:4,60:5,65:6,70:7,75:8,95:100"), None);
    }

    #[test]
    fn temperatures_rise_and_percentages_never_fall() {
        for bad in ["60:20,60:100", "60:20,59:100", "60:50,70:49,90:100", "60:20,70:30,70:100"] {
            assert_eq!(FanCurve::parse(bad), None, "{bad}");
        }
        // One degree apart and equal percentages are fine.
        assert_eq!(curve("60:20,61:100").points().len(), 2);
        assert_eq!(curve("60:50,70:50,90:100").pct_at(65.0), 50);
        assert_eq!(curve("60:100,70:100").pct_at(60.0), 100);
    }

    #[test]
    fn syntax_edge_cases_rejected() {
        for bad in [
            ",60:20,90:100",
            "60:20,90:100,",
            "60:20, ,90:100",
            ",",
            " ",
            "60:",
            ":100",
            "60:20,90",
            "60:20:30,90:100",
            "60:20;90:100",
            "60 20,90:100",
            "-60:100",
            "60:-1,90:100",
            "300:100",
            "60:300",
            "60:256,90:100",
            "99999999999:100",
            "60.0:20,90:100",
            "60:20.5,90:100",
            "0x3C:100",
            "\u{ff16}\u{ff10}:100",
            "60\u{b0}:100",
            "90:100%",
            "\u{663}\u{660}:100",
        ] {
            assert_eq!(FanCurve::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn whitespace_around_numbers_is_ignored() {
        assert_eq!(curve(" 60 : 20 ,\t90 :100\n").points(), &[(60, 20), (90, 100)]);
        assert_eq!(curve("60:20,\r\n90:100").points(), &[(60, 20), (90, 100)]);
    }

    #[test]
    fn pct_at_points_and_between() {
        for c in [old_balanced(), old_eco(), FanCurve::balanced_default(), FanCurve::eco_default()] {
            for &(t, p) in c.points() {
                assert_eq!(c.pct_at(f32::from(t)), p, "{c} at {t}");
            }
            assert_eq!(c.pct_at(f32::from(c.start()) - 0.001), 0, "{c}");
        }
        let c = old_balanced();
        assert_eq!(c.pct_at(89.5), 90);
        assert_eq!(c.pct_at(91.99), 100);
        assert_eq!(c.pct_at(95.0), 100);
        assert_eq!(c.pct_at(f32::MAX), 100);
    }

    #[test]
    fn pct_at_non_finite_and_negative() {
        for c in [old_balanced(), curve("70:100"), curve("40:0,95:100")] {
            assert_eq!(c.pct_at(-1.0), 0);
            assert_eq!(c.pct_at(-273.15), 0);
            assert_eq!(c.pct_at(f32::MIN), 0);
            assert_eq!(c.pct_at(f32::NEG_INFINITY), 0);
            assert_eq!(c.pct_at(f32::INFINITY), 100);
            // Fails safe: full speed, no panic.
            assert_eq!(c.pct_at(f32::NAN), 100);
        }
    }

    #[test]
    fn map_pct_validates_the_result() {
        let c = old_balanced();
        assert_eq!(c.map_pct(|p| p), Some(c.clone()));
        assert_eq!(c.map_pct(|_| 100).map(|m| m.to_string()).as_deref(), Some("60:100,70:100,80:100,87:100,92:100"));
        assert_eq!(c.map_pct(|p| p + 1), None); // last point 101 %
        assert_eq!(c.map_pct(|p| 100 - p), None); // falling
        assert_eq!(c.map_pct(|_| 0), None);
        assert_eq!(curve("90:100").map_pct(|p| p - 1), None);
    }

    #[test]
    fn display_parse_round_trip() {
        for c in [FanCurve::balanced_default(), FanCurve::eco_default(), old_balanced(), old_eco()] {
            assert_eq!(FanCurve::parse(&c.to_string()).as_ref(), Some(&c));
        }
        let mut seed = 0x2545_f491_u32;
        let mut next = move |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % n
        };
        for _ in 0..1000 {
            let mut temps: Vec<u8> = (40..=95).filter(|_| next(8) == 0).take(8).collect();
            if temps.is_empty() {
                temps.push(95);
            }
            let mut pcts: Vec<u8> = temps.iter().map(|_| u8::try_from(next(101)).unwrap()).collect();
            pcts.sort_unstable();
            *pcts.last_mut().unwrap() = 100;
            let text: Vec<String> = temps.iter().zip(&pcts).map(|(t, p)| format!("{t}:{p}")).collect();
            let text = text.join(",");
            let c = curve(&text);
            assert_eq!(c.to_string(), text);
            assert_eq!(FanCurve::parse(&c.to_string()), Some(c));
        }
    }

    #[test]
    fn fresh_track_has_no_history() {
        let t = FanTrack::default();
        assert_eq!((t.pct(), t.temp()), (0, None));
        let c = old_balanced();
        let mut t = FanTrack::default();
        assert_eq!(t.step(&c, 95.0), 100);
        assert!(ema_is(&t, 95.0));
        // Just inside the stop band: no history, so the fans are off.
        assert_eq!(run(&c, &[58.0]), [0]);
        assert_eq!(run(&c, &[60.0]), [25]);
    }

    #[test]
    fn rising_temperature_raises_speed_every_tick() {
        let s = run(&old_balanced(), &[60.0, 80.0, 80.0, 80.0, 80.0, 80.0]);
        assert_eq!(s[0], 25);
        assert!(s.windows(2).all(|w| w[0] < w[1]), "{s:?}");
    }

    #[test]
    fn raw_temperature_used_from_fast_at() {
        let c = old_balanced();
        assert_eq!(run(&c, &[50.0, FAST_AT - 0.1]), [0, 0]);
        assert_eq!(run(&c, &[50.0, FAST_AT]), [0, 92]);
        let mut t = FanTrack::default();
        t.step(&c, 50.0);
        t.step(&c, 93.0);
        assert!(ema_is(&t, 93.0));
    }

    #[test]
    fn short_spike_while_running_is_held_then_eased_off() {
        // 70 °C = 35 %; a one-tick spike to 88 °C smooths to 74.5 °C = 44 %.
        assert_eq!(run(&old_balanced(), &[70.0, 88.0, 70.0, 70.0, 70.0]), [35, 44, 44, 44, 39]);
    }

    #[test]
    fn back_from_fast_at_ramps_down_in_steps() {
        assert_eq!(run(&old_balanced(), &[50.0, 95.0, 50.0, 50.0, 50.0]), [0, 100, 90, 80, 70]);
    }

    #[test]
    fn fast_cool_down_only_stops_from_the_minimum() {
        let c = old_balanced();
        let mut raws = vec![95.0];
        raws.extend([50.0; 20]);
        let s = run(&c, &raws);
        let min = c.points()[0].1;
        assert!(s.windows(2).all(|w| w[0] <= w[1] || w[0] - w[1] <= DOWN_STEP || (w[0] <= min && w[1] == 0)), "{s:?}");
    }

    #[test]
    fn stop_hysteresis_edge() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        t.step(&c, 60.0);
        // 60 + 0.25 * (48 - 60) = 57 = start - STOP_HYSTERESIS: not yet clearly cooler.
        assert_eq!(t.step(&c, 48.0), 25);
        assert!(ema_is(&t, f32::from(c.start()) - STOP_HYSTERESIS));
        assert_eq!(t.step(&c, 56.9), 0);
        // Off fans stay off in the band; they start again at the start temperature.
        assert_eq!(t.step(&c, 59.9), 0);
        assert!(t.temp().is_some_and(|e| e < 60.0));
        assert_eq!(t.step(&c, 70.0), 26);
    }

    #[test]
    fn hovering_at_the_start_does_not_flap() {
        let raws: Vec<f32> = (0..40).map(|i| if i % 2 == 0 { 61.0 } else { 58.0 }).collect();
        let s = run(&old_balanced(), &raws);
        assert!(s.iter().all(|&p| (25..=26).contains(&p)), "{s:?}");
    }

    #[test]
    fn ramp_down_deadband_and_step_edges() {
        let c = old_balanced();
        // From 55 % at 80 °C, one tick each.
        assert_eq!(run(&c, &[80.0, 72.0]), [55, 55], "78 °C = 51 %: within the deadband");
        assert_eq!(run(&c, &[80.0, 70.0]), [55, 50], "77.5 °C = 50 %: just outside");
        assert_eq!(run(&c, &[80.0, 64.0]), [55, 47], "76 °C = 47 %: less than a step");
        assert_eq!(run(&c, &[80.0, 60.0]), [55, 45], "75 °C = 45 %: exactly a step");
        assert_eq!(run(&c, &[80.0, 56.0]), [55, 45], "74 °C = 43 %: capped at a step");
        assert_eq!(DOWN_STEP, 55 - 45);
        assert_eq!(DOWN_DEADBAND, 55 - 51);
    }

    #[test]
    fn long_sequences_converge() {
        let c = old_balanced();
        let mut t = FanTrack::default();
        t.step(&c, 50.0);
        for _ in 0..200 {
            t.step(&c, 75.0);
        }
        assert_eq!(t.pct(), 45);
        assert!(ema_is(&t, 75.0));
        // Coming down to the same load ends within the deadband above the target.
        t.step(&c, 95.0);
        for _ in 0..200 {
            t.step(&c, 75.0);
        }
        assert!((45..=45 + DOWN_DEADBAND).contains(&t.pct()), "{}", t.pct());
        for _ in 0..200 {
            t.step(&c, 40.0);
        }
        assert_eq!(t.pct(), 0);
        assert!(ema_is(&t, 40.0));
    }

    #[test]
    fn non_finite_readings_fail_safe() {
        let c = old_balanced();
        assert_eq!(run(&c, &[f32::NAN]), [100]);
        assert_eq!(run(&c, &[50.0, f32::NAN]), [0, 100]);
        assert_eq!(run(&c, &[50.0, f32::INFINITY]), [0, 100]);
        assert_eq!(run(&c, &[f32::NEG_INFINITY]), [0]);
    }
}
