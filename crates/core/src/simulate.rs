//! Plays a recorded log through a pipeline the way the live filter would.

use crate::event_log::RawEvent;
use crate::pipeline::{Pipeline, PipelineConfig};

/// Rate of the ticker that finishes tails: 1 kHz.
const TICK_US: u64 = 1_000;

/// Returns the events the filter would have sent to the system. Buttons and
/// the wheel are copied through untouched.
pub fn simulate(events: &[RawEvent], config: &PipelineConfig) -> Vec<RawEvent> {
    let mut pipeline = Pipeline::new(config);
    let mut output = Vec::with_capacity(events.len());
    let mut buttons = 0;
    let mut next_tick = 0;

    for event in events {
        while next_tick < event.t_us {
            if !pipeline.is_settled() {
                let (dx, dy) = pipeline.tick(next_tick);
                push_motion(&mut output, next_tick, dx, dy, buttons);
            }
            next_tick += TICK_US;
        }

        let (dx, dy) = if event.is_motion() {
            pipeline.packet(event.t_us, event.dx, event.dy)
        } else {
            (0, 0)
        };
        let input_only = event.buttons != buttons || event.wheel != 0;
        buttons = event.buttons;
        if input_only || dx != 0 || dy != 0 {
            output.push(RawEvent {
                t_us: event.t_us,
                dx,
                dy,
                buttons,
                wheel: event.wheel,
            });
        }
    }

    while !pipeline.is_settled() {
        let (dx, dy) = pipeline.tick(next_tick);
        push_motion(&mut output, next_tick, dx, dy, buttons);
        next_tick += TICK_US;
    }
    output
}

fn push_motion(output: &mut Vec<RawEvent>, t_us: u64, dx: i32, dy: i32, buttons: u32) {
    if dx != 0 || dy != 0 {
        output.push(RawEvent {
            t_us,
            dx,
            dy,
            buttons,
            wheel: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::parse_spec;

    fn motion(t_us: u64, dx: i32) -> RawEvent {
        RawEvent {
            t_us,
            dx,
            ..Default::default()
        }
    }

    #[test]
    fn passthrough_reproduces_the_log() {
        let events = [
            motion(1_000, 3),
            RawEvent {
                t_us: 9_000,
                dx: -2,
                dy: 1,
                buttons: 1,
                wheel: 0,
            },
            RawEvent {
                t_us: 17_000,
                buttons: 1,
                wheel: -120,
                ..Default::default()
            },
        ];
        assert_eq!(simulate(&events, &PipelineConfig::default()), events);
    }

    #[test]
    fn clicks_keep_their_time_while_motion_is_smoothed() {
        let click = RawEvent {
            t_us: 16_000,
            buttons: 1,
            ..Default::default()
        };
        let events = [motion(8_000, 50), click];
        let config = parse_spec("ema:tau_ms=100").unwrap();
        let output = simulate(&events, &config);
        assert!(output.iter().any(|e| e.t_us == 16_000 && e.buttons == 1));
    }

    #[test]
    fn smoothing_adds_a_tail_after_the_last_packet() {
        let events: Vec<RawEvent> = (1..10).map(|i| motion(i * 8_000, 10)).collect();
        let config = parse_spec("ema:tau_ms=10").unwrap();
        let output = simulate(&events, &config);
        assert!(output.last().unwrap().t_us > 72_000);
        let total: i32 = output.iter().map(|e| e.dx).sum();
        assert_eq!(total, 90);
    }
}
