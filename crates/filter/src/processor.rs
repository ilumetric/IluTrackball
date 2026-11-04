//! Runs the pipeline for the input and ticker threads.
//!
//! Losing the cursor is the one failure that must not happen, so any panic in
//! the pipeline turns the processor into a plain passthrough.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ilumx_core::pipeline::{Pipeline, PipelineConfig};

use crate::tap::{Flow, Tap, TapSample};

pub struct Processor {
    pipeline: Mutex<Pipeline>,
    next_config: Mutex<Option<PipelineConfig>>,
    config_changed: AtomicBool,
    failed: AtomicBool,
    pub tap: Tap,
}

impl Processor {
    pub fn new(config: &PipelineConfig) -> Self {
        Self {
            pipeline: Mutex::new(Pipeline::new(config)),
            next_config: Mutex::new(None),
            config_changed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            tap: Tap::default(),
        }
    }

    /// Takes effect with the next packet or tick. Motion still held by the old
    /// pipeline is released at that moment.
    pub fn set_config(&self, config: PipelineConfig) {
        if let Ok(mut next) = self.next_config.lock() {
            *next = Some(config);
            self.config_changed.store(true, Ordering::Release);
        }
    }

    pub fn packet(&self, t_us: u64, dx: i32, dy: i32) -> (i32, i32) {
        self.tap.push(TapSample {
            t_us,
            flow: Flow::Input,
            dx,
            dy,
        });
        self.guarded(t_us, (dx, dy), |pipeline| pipeline.packet(t_us, dx, dy))
    }

    pub fn tick(&self, t_us: u64) -> (i32, i32) {
        self.guarded(t_us, (0, 0), |pipeline| pipeline.tick(t_us))
    }

    pub fn flush(&self, t_us: u64) -> (i32, i32) {
        self.guarded(t_us, (0, 0), |pipeline| pipeline.flush(t_us))
    }

    /// True when the ticker may go to sleep.
    pub fn is_settled(&self) -> bool {
        if self.config_changed.load(Ordering::Acquire) {
            return false;
        }
        match self.pipeline.lock() {
            Ok(pipeline) => pipeline.is_settled(),
            Err(_) => true,
        }
    }

    /// True after a panic: motion now passes unchanged.
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    fn guarded(
        &self,
        t_us: u64,
        fallback: (i32, i32),
        action: impl FnOnce(&mut Pipeline) -> (i32, i32),
    ) -> (i32, i32) {
        if self.has_failed() {
            return fallback;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut pipeline = self.pipeline.lock().expect("pipeline lock poisoned");
            let released = self.apply_new_config(&mut pipeline, t_us);
            let out = action(&mut pipeline);
            (out.0 + released.0, out.1 + released.1)
        }));
        result.unwrap_or_else(|_| {
            self.failed.store(true, Ordering::Relaxed);
            fallback
        })
    }

    fn apply_new_config(&self, pipeline: &mut Pipeline, t_us: u64) -> (i32, i32) {
        if !self.config_changed.swap(false, Ordering::Acquire) {
            return (0, 0);
        }
        let next = self.next_config.lock().ok().and_then(|mut next| next.take());
        let Some(config) = next else {
            return (0, 0);
        };
        let released = pipeline.flush(t_us);
        *pipeline = Pipeline::new(&config);
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ilumx_core::spec::parse_spec;
    use std::sync::Arc;
    use std::thread;

    fn processor(spec: &str) -> Processor {
        Processor::new(&parse_spec(spec).unwrap())
    }

    #[test]
    fn passthrough_returns_motion_unchanged() {
        let processor = processor("passthrough");
        assert_eq!(processor.packet(1_000, 5, -3), (5, -3));
        assert_eq!(processor.tick(2_000), (0, 0));
        assert!(processor.is_settled());
    }

    #[test]
    fn smoothing_needs_ticks_to_finish() {
        let processor = processor("ema:tau_ms=10");
        assert_eq!(processor.packet(0, 100, 0), (0, 0));
        assert!(!processor.is_settled());
        let mut total = 0;
        let mut t = 0;
        while !processor.is_settled() {
            t += 1_000;
            total += processor.tick(t).0;
        }
        assert!((99..=100).contains(&total), "{total}");
    }

    #[test]
    fn a_new_config_releases_held_motion_and_takes_over() {
        let processor = processor("ema:tau_ms=1000");
        assert_eq!(processor.packet(0, 50, 0), (0, 0));
        processor.set_config(parse_spec("passthrough").unwrap());
        assert!(!processor.is_settled());
        assert_eq!(processor.packet(1_000, 7, 0), (57, 0));
        assert_eq!(processor.packet(2_000, 3, 0), (3, 0));
    }

    #[test]
    fn a_new_config_reaches_the_ticker_too() {
        let processor = processor("ema:tau_ms=1000");
        processor.packet(0, 50, 0);
        processor.set_config(parse_spec("passthrough").unwrap());
        assert_eq!(processor.tick(1_000), (50, 0));
        assert!(processor.is_settled());
    }

    #[test]
    fn a_panic_turns_the_processor_into_passthrough() {
        let processor = Arc::new(processor("ema:tau_ms=10"));
        let poisoner = Arc::clone(&processor);
        let _ = thread::spawn(move || {
            let _held = poisoner.pipeline.lock().unwrap();
            panic!("simulated failure in the pipeline");
        })
        .join();

        assert_eq!(processor.packet(1_000, 9, -2), (9, -2));
        assert!(processor.has_failed());
        assert_eq!(processor.packet(2_000, 4, 1), (4, 1));
        assert_eq!(processor.tick(3_000), (0, 0));
        assert!(processor.is_settled());
    }

    #[test]
    fn packets_reach_the_tap() {
        let processor = processor("passthrough");
        processor.packet(1_000, 2, 3);
        let samples = processor.tap.drain();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].flow, Flow::Input);
        assert_eq!((samples[0].dx, samples[0].dy), (2, 3));
    }
}
