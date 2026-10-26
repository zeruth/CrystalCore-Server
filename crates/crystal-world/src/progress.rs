//! Class levels and experience.

/// The β's level cap.
pub const MAX_LEVEL: u8 = 50;

/// A class's level and the experience gained toward the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub level: u8,
    pub exp: u32,
}

impl Default for Progress {
    fn default() -> Self {
        Self { level: 1, exp: 0 }
    }
}

impl Progress {
    /// Adds `amount` experience, levelling up as `to_next` (the experience a level needs: the
    /// `ParamGrow` table) allows. At the cap experience stops. The levels gained.
    pub fn gain(&mut self, amount: u32, to_next: impl Fn(u8) -> Option<u32>) -> u8 {
        let start = self.level;
        let mut exp = self.exp as u64 + amount as u64;
        while self.level < MAX_LEVEL {
            let Some(need) = to_next(self.level) else { break };
            if exp < need as u64 {
                break;
            }
            exp -= need as u64;
            self.level += 1;
        }
        self.exp = if self.level >= MAX_LEVEL { 0 } else { exp.min(u32::MAX as u64) as u32 };
        self.level - start
    }

    /// Sets the level outright (a GM's doing), keeping no experience toward the next.
    pub fn set_level(&mut self, level: u8) {
        self.level = level.clamp(1, MAX_LEVEL);
        self.exp = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ParamGrow's first rows in the β: 300, 600, then made up
    fn table(level: u8) -> Option<u32> {
        Some(match level {
            1 => 300,
            2 => 600,
            _ => 1000,
        })
    }

    #[test]
    fn levels_up_and_carries_the_rest() {
        let mut p = Progress::default();
        assert_eq!(p.gain(299, table), 0);
        assert_eq!(p, Progress { level: 1, exp: 299 });
        assert_eq!(p.gain(1, table), 1);
        assert_eq!(p, Progress { level: 2, exp: 0 });
        // several levels at once, the rest kept
        assert_eq!(p.gain(600 + 1000 + 1000 + 7, table), 3);
        assert_eq!(p, Progress { level: 5, exp: 7 });
    }

    #[test]
    fn stops_at_the_cap_and_at_the_table_end() {
        let mut p = Progress { level: MAX_LEVEL - 1, exp: 0 };
        p.gain(u32::MAX, table);
        assert_eq!(p, Progress { level: MAX_LEVEL, exp: 0 });
        assert_eq!(p.gain(5000, table), 0);
        let mut short = Progress::default();
        short.gain(10_000, |l| (l < 3).then_some(10));
        assert_eq!(short.level, 3, "no row for level 3: it stays");
        p.set_level(0);
        assert_eq!(p.level, 1);
        p.set_level(200);
        assert_eq!(p.level, MAX_LEVEL);
    }
}
