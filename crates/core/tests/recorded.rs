//! Runs the pipeline over logs recorded from the real device.

use std::path::Path;

use ilumx_core::event_log::{read_csv, RawEvent};
use ilumx_core::pipeline::{Pipeline, PipelineConfig};
use ilumx_core::spec::parse_spec;

const TICK_US: u64 = 1_000;

fn load(name: &str) -> Vec<RawEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    read_csv(&path).unwrap()
}

/// Plays a log with a 1 kHz ticker and returns the total motion emitted.
fn replay_total(events: &[RawEvent], config: &PipelineConfig) -> (i64, i64) {
    let mut pipeline = Pipeline::new(config);
    let mut total = (0i64, 0i64);
    let mut add = |out: (i32, i32)| {
        total.0 += out.0 as i64;
        total.1 += out.1 as i64;
    };

    let mut next_tick = 0;
    for event in events {
        while next_tick < event.t_us {
            add(pipeline.tick(next_tick));
            next_tick += TICK_US;
        }
        if event.is_motion() {
            add(pipeline.packet(event.t_us, event.dx, event.dy));
        }
    }
    while !pipeline.is_settled() {
        add(pipeline.tick(next_tick));
        next_tick += TICK_US;
    }
    total
}

fn input_total(events: &[RawEvent]) -> (i64, i64) {
    events.iter().fold((0, 0), |total, event| {
        (total.0 + event.dx as i64, total.1 + event.dy as i64)
    })
}

#[test]
fn passthrough_is_bit_exact_on_real_logs() {
    for name in ["circle.csv", "slow_h.csv"] {
        let events = load(name);
        let mut pipeline = Pipeline::new(&PipelineConfig::default());
        for event in events.iter().filter(|event| event.is_motion()) {
            let out = pipeline.packet(event.t_us, event.dx, event.dy);
            assert_eq!(out, (event.dx, event.dy));
        }
    }
}

#[test]
fn no_mode_loses_or_invents_motion_on_real_logs() {
    let specs = [
        "ema:tau_ms=8",
        "ema:tau_ms=40",
        "one_euro",
        "debeat",
        "debeat+ema:tau_ms=5",
        "debeat+spread",
    ];
    for name in ["circle.csv", "slow_h.csv"] {
        let events = load(name);
        let expected = input_total(&events);
        for spec in specs {
            let config = parse_spec(spec).unwrap();
            let total = replay_total(&events, &config);
            // Held motion under the settle limit may stay behind at the end.
            assert!(
                (total.0 - expected.0).abs() <= 1 && (total.1 - expected.1).abs() <= 1,
                "{name} {spec}: {total:?} vs {expected:?}"
            );
        }
    }
}
