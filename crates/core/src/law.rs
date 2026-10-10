//! The range-and-step arithmetic every adapter needs once it moves a control
//! by a *relative* amount on a protocol that only takes absolute values.
//!
//! Most desk protocols this project speaks have no increment/decrement for
//! trim (see `docs/GAIN-LOCK.md` for the survey), so a relative move is a
//! read, this arithmetic, and a write. The vendor knowledge - the actual range
//! and step - stays in each adapter as a `DbLaw` constant taken from its
//! protocol document; only the arithmetic lives here.

/// A control that holds dB values on a linear grid: `min_db..=max_db` in
/// steps of `step_db`, the grid anchored at `min_db`. X32's `linf [-18.000,
/// 18.000, 0.250]` is `DbLaw::new(-18.0, 18.0, 0.25)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DbLaw {
    pub min_db: f32,
    pub max_db: f32,
    pub step_db: f32,
}

impl DbLaw {
    pub const fn new(min_db: f32, max_db: f32, step_db: f32) -> Self {
        Self { min_db, max_db, step_db }
    }

    /// The nearest value the control can actually hold.
    pub fn quantize(&self, db: f32) -> f32 {
        self.at(self.index(db))
    }

    /// Which grid point `db` lands on, counted from `min_db`. Moves are
    /// worked out in whole steps so a 0.1 dB grid doesn't leave float dust
    /// (`-2.199999` for a 22-step move).
    fn index(&self, db: f32) -> i64 {
        let (min, step) = (self.min_db as f64, self.step_db as f64);
        ((db.clamp(self.min_db, self.max_db) as f64 - min) / step).round() as i64
    }

    /// In f64 to keep the error to the f32 step itself (0.1 isn't exact):
    /// 173 steps from -18 lands within 3e-7 of -0.7, not 1.2e-6. The desk
    /// snaps to its own grid either way; it's the step count that matters.
    fn at(&self, index: i64) -> f32 {
        let db = self.min_db as f64 + index as f64 * self.step_db as f64;
        (db as f32).clamp(self.min_db, self.max_db)
    }

    /// Where a move of `delta_db` from `current_db` lands, and the change that
    /// actually is: `(target, applied)`. `applied` is short of `delta_db` at a
    /// range limit and differs by up to half a step everywhere else - the
    /// caller keeps the difference rather than losing it (see
    /// [`crate::GainLock`]'s `owed`).
    pub fn nudge(&self, current_db: f32, delta_db: f32) -> (f32, f32) {
        let current = self.index(current_db);
        let target = self.index(self.at(current) + delta_db);
        (self.at(target), (target - current) as f32 * self.step_db)
    }

    /// Whether `delta_db` moves the control at all from an on-grid value -
    /// lets an adapter skip the read-modify-write round trip for a move it
    /// would round away.
    pub fn moves(&self, delta_db: f32) -> bool {
        delta_db.abs() >= self.step_db / 2.0
    }

    /// `db` as the 0.0..=1.0 float a protocol with normalised floats (X32,
    /// the Wing's raw value) expects for a linear control.
    pub fn to_unit(&self, db: f32) -> f32 {
        (self.quantize(db) - self.min_db) / (self.max_db - self.min_db)
    }

    /// The inverse of [`to_unit`](Self::to_unit), snapped to the grid.
    pub fn from_unit(&self, unit: f32) -> f32 {
        self.quantize(self.min_db + unit.clamp(0.0, 1.0) * (self.max_db - self.min_db))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X32_TRIM: DbLaw = DbLaw::new(-18.0, 18.0, 0.25);

    #[test]
    fn quantize_snaps_to_the_grid_and_clamps() {
        assert_eq!(X32_TRIM.quantize(1.1), 1.0);
        assert_eq!(X32_TRIM.quantize(1.2), 1.25);
        assert_eq!(X32_TRIM.quantize(40.0), 18.0);
        assert_eq!(X32_TRIM.quantize(-40.0), -18.0);
    }

    #[test]
    fn nudge_reports_what_was_actually_applied() {
        assert_eq!(X32_TRIM.nudge(0.0, -3.0), (-3.0, -3.0));
        // Half a step either side of the grid rounds to the nearest point.
        assert_eq!(X32_TRIM.nudge(0.0, -0.433), (-0.5, -0.5));
        // At the limit the move is cut short, and says so.
        assert_eq!(X32_TRIM.nudge(17.0, 3.0), (18.0, 1.0));
        assert_eq!(X32_TRIM.nudge(-18.0, -1.0), (-18.0, 0.0));
    }

    #[test]
    fn a_fine_grid_move_is_a_whole_number_of_steps() {
        let wing = DbLaw::new(-18.0, 18.0, 0.1);
        let (target, applied) = wing.nudge(1.5, -2.2);
        assert_eq!(applied, -2.2);
        assert!((target - -0.7).abs() < 1e-5, "{target}");
    }

    #[test]
    fn moves_is_false_under_half_a_step() {
        assert!(!X32_TRIM.moves(0.1));
        assert!(X32_TRIM.moves(0.125));
        assert!(X32_TRIM.moves(-0.2));
    }

    #[test]
    fn unit_conversion_round_trips_on_grid_values() {
        let headamp = DbLaw::new(-12.0, 60.0, 0.5);
        assert_eq!(headamp.to_unit(-12.0), 0.0);
        assert_eq!(headamp.to_unit(60.0), 1.0);
        assert_eq!(headamp.to_unit(24.0), 0.5);
        for db in [-12.0, -0.5, 0.0, 23.5, 59.5, 60.0] {
            assert_eq!(headamp.from_unit(headamp.to_unit(db)), db);
        }
        assert_eq!(X32_TRIM.to_unit(0.0), 0.5);
    }
}
