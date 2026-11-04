//! Hands motion samples from the input threads to the settings window.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Enough for several seconds of packets and ticks between two window frames.
const CAPACITY: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Motion that came from the trackball.
    Input,
    /// Motion that went out to the system.
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapSample {
    pub t_us: u64,
    pub flow: Flow,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Default)]
pub struct Tap {
    samples: Mutex<VecDeque<TapSample>>,
}

impl Tap {
    /// Never waits: the input threads must not depend on the window, so a
    /// sample is dropped if the window holds the queue at that moment.
    pub fn push(&self, sample: TapSample) {
        if let Ok(mut samples) = self.samples.try_lock() {
            if samples.len() == CAPACITY {
                samples.pop_front();
            }
            samples.push_back(sample);
        }
    }

    pub fn drain(&self) -> Vec<TapSample> {
        self.samples
            .lock()
            .map(|mut samples| samples.drain(..).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_us: u64) -> TapSample {
        TapSample {
            t_us,
            flow: Flow::Input,
            dx: 1,
            dy: 0,
        }
    }

    #[test]
    fn drain_returns_samples_in_order_and_empties_the_queue() {
        let tap = Tap::default();
        tap.push(sample(1));
        tap.push(sample(2));
        assert_eq!(tap.drain(), [sample(1), sample(2)]);
        assert!(tap.drain().is_empty());
    }

    #[test]
    fn a_window_that_stopped_reading_only_loses_old_samples() {
        let tap = Tap::default();
        for t in 0..(CAPACITY as u64 + 5) {
            tap.push(sample(t));
        }
        let samples = tap.drain();
        assert_eq!(samples.len(), CAPACITY);
        assert_eq!(samples[0].t_us, 5);
    }
}
