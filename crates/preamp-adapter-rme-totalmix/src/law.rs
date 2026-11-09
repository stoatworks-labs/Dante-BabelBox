//! TotalMix FX's fader law: a fader position as Global OSC's `faderlin`
//! (0.0..=1.0) and the level in dB it stands for.
//!
//! The fader has 1024 positions (0..=1023). From position 649 (-6 dB) to the
//! top (1023, +6 dB) it is linear in dB. Below 649 it is a parabola that
//! reaches -65 dB at position 0, which TotalMix shows as -inf.
//!
//! Source: the `rme-totalmix` Companion module 1.0.1 (MIT), its `$()`
//! (position to dB) and `ee()` (dB to position) functions, ported as they
//! are. The constants check out at both ends and at the knee: 1023 gives
//! +6.000 dB, 649 gives -6.000 dB from both halves, and 0 gives -65 dB.

/// TotalMix's bottom. It and anything below it is off (-inf).
pub const MIN_DB: f32 = -65.0;
/// TotalMix's top, +6 dB.
pub const MAX_DB: f32 = 6.0;
/// The highest fader position; `faderlin` is `position / 1023`.
pub const POSITIONS: u16 = 1023;

const KNEE: f64 = 649.0;
const SLOPE: f64 = 0.032_085_561_5;
const OFFSET: f64 = 26.823_529_411_8;

/// The fader position for `db`, rounded to the nearest of TotalMix's
/// 1024 steps and clamped to -65..=+6 dB.
pub fn position_for_db(db: f32) -> u16 {
    if db.is_nan() || db <= MIN_DB {
        return 0;
    }
    if db >= MAX_DB {
        return POSITIONS;
    }
    let e = db as f64;
    let t = if e >= -6.0 {
        (e + OFFSET) / SLOPE
    } else {
        826.0 - (-34_869.0 - 11_033.0 * e).sqrt()
    };
    t.round().clamp(0.0, POSITIONS as f64) as u16
}

/// The level in dB of fader position `position` (0..=1023).
pub fn db_for_position(position: u16) -> f32 {
    let t = position.min(POSITIONS) as f64;
    let db = if t >= KNEE {
        t * SLOPE - OFFSET
    } else {
        t * t * (-1.0 / 11_033.0) + t * 0.149_732_620_3 - 65.0
    };
    (db as f32).clamp(MIN_DB, MAX_DB)
}

/// `faderlin` for a position.
pub fn faderlin(position: u16) -> f32 {
    position.min(POSITIONS) as f32 / POSITIONS as f32
}

/// The position nearest to a `faderlin` value TotalMix reported.
pub fn position_for_faderlin(value: f32) -> u16 {
    if value.is_nan() {
        return 0;
    }
    (value.clamp(0.0, 1.0) * POSITIONS as f32).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_and_the_knee_are_where_totalmix_puts_them() {
        assert_eq!(db_for_position(0), MIN_DB);
        assert!((db_for_position(1023) - 6.0).abs() < 1e-4);
        assert!((db_for_position(649) - -6.0).abs() < 1e-4);
        // Both halves of the law meet at the knee.
        let lower = 649.0_f64 * 649.0 * (-1.0 / 11_033.0) + 649.0 * 0.149_732_620_3 - 65.0;
        assert!((lower - -6.0).abs() < 1e-4, "{lower}");
    }

    #[test]
    fn zero_db_is_position_836() {
        assert_eq!(position_for_db(0.0), 836);
        assert!(db_for_position(836).abs() < 0.02);
    }

    #[test]
    fn every_position_survives_a_round_trip_through_db() {
        for p in 0..=POSITIONS {
            assert_eq!(position_for_db(db_for_position(p)), p, "position {p}");
        }
    }

    #[test]
    fn out_of_range_levels_clamp_and_nan_is_off() {
        assert_eq!(position_for_db(-200.0), 0);
        assert_eq!(position_for_db(MIN_DB), 0);
        assert_eq!(position_for_db(40.0), POSITIONS);
        assert_eq!(position_for_db(f32::NAN), 0);
    }

    #[test]
    fn faderlin_round_trips_on_every_position() {
        for p in 0..=POSITIONS {
            assert_eq!(position_for_faderlin(faderlin(p)), p);
        }
        assert_eq!(position_for_faderlin(-1.0), 0);
        assert_eq!(position_for_faderlin(2.0), POSITIONS);
    }
}
