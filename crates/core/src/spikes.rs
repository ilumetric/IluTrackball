//! Finds packets that stand out from their neighbours in size.
//!
//! Over Bluetooth the MX Ergo's sensor and radio tick at different rates, so
//! now and then one packet carries two sensor frames. Such a packet is about
//! twice the size of its neighbours with no deficit around it.

/// Packets compared on each side of a candidate.
const NEIGHBOURS: usize = 5;
/// Below this typical size a doubled packet cannot be told from rounding.
const MIN_MEDIAN_COUNTS: f64 = 1.5;
const MIN_EXCESS_COUNTS: f64 = 2.0;
const MIN_EXCESS_RATIO: f64 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spike {
    pub index: usize,
    /// Size of the packet relative to the median of its neighbours.
    pub ratio: f64,
}

/// Scan result for one run of consecutive packets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpikeScan {
    pub spikes: Vec<Spike>,
    /// Packets that moved enough for a spike to be detectable at all.
    pub checked: usize,
}

/// Scans packet sizes of one uninterrupted run.
pub fn find_spikes(sizes: &[f64]) -> SpikeScan {
    let mut scan = SpikeScan::default();
    if sizes.len() <= NEIGHBOURS * 2 {
        return scan;
    }
    for i in NEIGHBOURS..sizes.len() - NEIGHBOURS {
        let median = neighbour_median(sizes, i);
        if median < MIN_MEDIAN_COUNTS {
            continue;
        }
        scan.checked += 1;
        let excess = sizes[i] - median;
        let stands_out = excess >= MIN_EXCESS_COUNTS.max(MIN_EXCESS_RATIO * median);
        let local_peak = sizes[i] > sizes[i - 1] && sizes[i] >= sizes[i + 1];
        if stands_out && local_peak {
            scan.spikes.push(Spike {
                index: i,
                ratio: sizes[i] / median,
            });
        }
    }
    scan
}

/// Median size of the packets around `i`. Needs `NEIGHBOURS` on each side.
pub fn neighbour_median(sizes: &[f64], i: usize) -> f64 {
    let mut around = [0.0; NEIGHBOURS * 2];
    around[..NEIGHBOURS].copy_from_slice(&sizes[i - NEIGHBOURS..i]);
    around[NEIGHBOURS..].copy_from_slice(&sizes[i + 1..=i + NEIGHBOURS]);
    around.sort_by(f64::total_cmp);
    (around[NEIGHBOURS - 1] + around[NEIGHBOURS]) / 2.0
}

/// How many packets on each side `neighbour_median` needs.
pub const fn neighbour_count() -> usize {
    NEIGHBOURS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_doubled_packet() {
        let sizes = [3.0, 3.0, 3.0, 3.0, 3.0, 6.0, 3.0, 3.0, 3.0, 3.0, 3.0];
        let scan = find_spikes(&sizes);
        assert_eq!(
            scan.spikes,
            [Spike {
                index: 5,
                ratio: 2.0
            }]
        );
        assert_eq!(scan.checked, 1);
    }

    #[test]
    fn ignores_steady_motion_and_rounding() {
        let sizes = [3.0, 2.0, 3.0, 3.0, 2.0, 3.0, 2.0, 3.0, 3.0, 2.0, 3.0, 3.0];
        assert!(find_spikes(&sizes).spikes.is_empty());
    }

    #[test]
    fn ignores_motion_too_slow_to_judge() {
        let sizes = [1.0, 0.0, 1.0, 1.0, 0.0, 3.0, 1.0, 0.0, 1.0, 1.0, 0.0];
        let scan = find_spikes(&sizes);
        assert!(scan.spikes.is_empty());
        assert_eq!(scan.checked, 0);
    }

    #[test]
    fn a_smooth_acceleration_is_not_a_spike() {
        let sizes: Vec<f64> = (1..30).map(f64::from).collect();
        assert!(find_spikes(&sizes).spikes.is_empty());
    }

    #[test]
    fn short_runs_are_skipped() {
        assert_eq!(find_spikes(&[1.0, 9.0, 1.0]), SpikeScan::default());
    }
}
