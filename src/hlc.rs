//! Hybrid logical clock (HLC).
//!
//! Varje app håller ett par (wall_ms, counter). Klockan är monoton även om
//! systemklockan hoppar bakåt, och flyttas fram när främmande händelser läses in.

use serde::{Deserialize, Serialize};

/// Främmande tider som ligger längre fram än så här jämfört med den lokala
/// systemklockan får inte flytta fram den lokala klockan.
pub const MAX_FUTURE_DRIFT_MS: u64 = 24 * 60 * 60 * 1000;

/// Räknaren skrivs med fyra siffror i filnamnet.
pub const MAX_COUNTER: u32 = 9999;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Clock {
    last: Hlc,
}

impl Clock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last(&self) -> Hlc {
        self.last
    }

    /// Tidsstämpel för en ny lokal händelse. Alltid strikt större än alla
    /// tidigare stämplar från den här klockan.
    pub fn tick(&mut self, now_ms: u64) -> Hlc {
        let next = if now_ms > self.last.wall_ms {
            Hlc {
                wall_ms: now_ms,
                counter: 0,
            }
        } else if self.last.counter < MAX_COUNTER {
            Hlc {
                wall_ms: self.last.wall_ms,
                counter: self.last.counter + 1,
            }
        } else {
            // Räknaren får inte bli bredare än filnamnet tillåter.
            Hlc {
                wall_ms: self.last.wall_ms + 1,
                counter: 0,
            }
        };
        self.last = next;
        next
    }

    /// Standard-HLC-merge med en främmande stämpel. Stämplar som ligger mer än
    /// 24 timmar före den lokala systemklockan ignoreras här (händelsen spelas
    /// ändå upp av anroparen).
    pub fn observe(&mut self, remote: Hlc, now_ms: u64) {
        if remote.wall_ms > now_ms.saturating_add(MAX_FUTURE_DRIFT_MS) {
            return;
        }
        if remote > self.last {
            self.last = remote;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_when_system_clock_goes_backwards() {
        let mut clock = Clock::new();
        let times = [
            1_000_000u64,
            1_000_005,
            999_000,
            999_000,
            500,
            1_000_005,
            1_000_006,
            0,
        ];
        let mut prev = clock.tick(times[0]);
        for &now in &times[1..] {
            let next = clock.tick(now);
            assert!(next > prev, "{next:?} ska vara större än {prev:?}");
            prev = next;
        }
    }

    #[test]
    fn counter_resets_when_wall_advances() {
        let mut clock = Clock::new();
        assert_eq!(
            clock.tick(100),
            Hlc {
                wall_ms: 100,
                counter: 0
            }
        );
        assert_eq!(
            clock.tick(100),
            Hlc {
                wall_ms: 100,
                counter: 1
            }
        );
        assert_eq!(
            clock.tick(101),
            Hlc {
                wall_ms: 101,
                counter: 0
            }
        );
    }

    #[test]
    fn counter_never_exceeds_file_name_width() {
        let mut clock = Clock::new();
        let mut prev = clock.tick(100);
        for _ in 0..25_000 {
            let next = clock.tick(100);
            assert!(next > prev);
            assert!(next.counter <= MAX_COUNTER);
            prev = next;
        }
    }

    #[test]
    fn merge_with_remote_time() {
        let now = 10_000_000u64;
        let mut clock = Clock::new();
        clock.tick(now);

        // Främmande klocka ligger en timme före: nästa lokala stämpel hamnar efter den.
        let remote = Hlc {
            wall_ms: now + 3_600_000,
            counter: 7,
        };
        clock.observe(remote, now);
        let next = clock.tick(now + 1);
        assert!(next > remote);
        assert_eq!(
            next,
            Hlc {
                wall_ms: remote.wall_ms,
                counter: 8
            }
        );

        // Äldre främmande stämplar backar aldrig klockan.
        clock.observe(
            Hlc {
                wall_ms: 5,
                counter: 0,
            },
            now,
        );
        assert!(clock.tick(now + 2) > next);
    }

    #[test]
    fn remote_more_than_24h_ahead_does_not_advance_clock() {
        let now = 10_000_000u64;
        let mut clock = Clock::new();
        let local = clock.tick(now);

        let far = Hlc {
            wall_ms: now + MAX_FUTURE_DRIFT_MS + 1,
            counter: 0,
        };
        clock.observe(far, now);
        assert_eq!(clock.last(), local);
        let next = clock.tick(now + 1);
        assert_eq!(next.wall_ms, now + 1);

        // Precis på gränsen accepteras.
        let edge = Hlc {
            wall_ms: now + MAX_FUTURE_DRIFT_MS,
            counter: 0,
        };
        clock.observe(edge, now);
        assert_eq!(clock.last(), edge);
    }
}
