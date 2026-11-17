//! The movement controller scene's re-dispatch. The player's movement comes from that scene's
//! reply to the tick dispatched at the start of the frame; a reply that lands after the frame's
//! scene loop deadline is applied a frame late, and the scene, still in flight when the next frame
//! queues its scenes, skips that frame's dispatch. When such a late reply lands, the scene is
//! dispatched again at once with the frame's input, ahead of the queue, if the time left before
//! the deadline covers the 90th percentile of its recent ticks.

use std::{collections::VecDeque, time::Duration};

use web_time::Instant;

/// Ticks the percentile is taken over.
pub const TICK_SAMPLES: usize = 32;

/// The movement scene's recent tick times, dispatch to reply.
#[derive(Debug, Default)]
pub struct MovementTickTimes {
    samples: VecDeque<Duration>,
}

impl MovementTickTimes {
    pub fn record(&mut self, tick: Duration) {
        if self.samples.len() == TICK_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(tick);
    }

    /// The 90th percentile (nearest rank) of the recorded ticks; None before the first.
    pub fn p90(&self) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let rank = (sorted.len() * 9).div_ceil(10);
        Some(sorted[rank.max(1) - 1])
    }

    /// A re-dispatch is worth it when the time left in the frame's scene loop covers the
    /// percentile; with no ticks recorded yet, always.
    pub fn redispatch_fits(&self, left: Duration) -> bool {
        self.p90().is_none_or(|p90| left >= p90)
    }
}

/// How long a tick took from its dispatch to its reply. The scene loop polls replies only while
/// it runs, so a reply first seen at a loop's first poll, from a tick dispatched in an earlier
/// frame, arrived some time after the previous loop ended: it counts as taking until then, the
/// least it took.
pub fn tick_time(
    sent: Instant,
    received: Instant,
    first_poll_of_later_frame: bool,
    previous_loop_end: Instant,
) -> Duration {
    let arrived = if first_poll_of_later_frame {
        previous_loop_end.min(received)
    } else {
        received
    };
    arrived.saturating_duration_since(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn p90_is_the_nearest_rank_over_the_last_samples() {
        let mut times = MovementTickTimes::default();
        assert_eq!(times.p90(), None);
        assert!(times.redispatch_fits(ms(0)));
        for v in 1..=10 {
            times.record(ms(v));
        }
        assert_eq!(times.p90(), Some(ms(9)));
        assert!(times.redispatch_fits(ms(9)));
        assert!(!times.redispatch_fits(ms(8)));
        // older samples drop out
        for _ in 0..TICK_SAMPLES {
            times.record(ms(2));
        }
        assert_eq!(times.p90(), Some(ms(2)));
    }

    #[test]
    fn a_reply_seen_at_a_later_loops_first_poll_counts_until_the_previous_loop_ended() {
        let sent = Instant::now();
        let loop_end = sent + ms(5);
        let received = sent + ms(16);
        assert_eq!(tick_time(sent, received, false, loop_end), ms(16));
        assert_eq!(tick_time(sent, received, true, loop_end), ms(5));
        // a loop that ended before the dispatch (the first frame) gives no negative time
        assert_eq!(tick_time(loop_end, received, true, sent), Duration::ZERO);
    }
}
