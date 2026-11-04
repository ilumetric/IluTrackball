//! A preset as the settings window edits it.
//!
//! Every part keeps its values while switched off, so switching it back on
//! restores what was there. Chains always come out in the same order.

use ilumx_core::curve::Curve;
use ilumx_core::debeat::DebeatConfig;
use ilumx_core::pipeline::{PipelineConfig, StageConfig};

use crate::curve_editor::MEDIUM_CURVE;

/// Where linear and power curves from the settings file are sampled, so the
/// window can show them as points on its graph.
const SAMPLE_SPEEDS: [f64; 10] = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0];

#[derive(Clone, Debug, PartialEq)]
pub struct PresetModel {
    pub debeat_on: bool,
    pub debeat: DebeatConfig,
    pub one_euro_on: bool,
    pub min_cutoff_hz: f64,
    pub beta: f64,
    pub d_cutoff_hz: f64,
    pub ema_on: bool,
    pub tau_ms: f64,
    pub spread_on: bool,
    pub spread_ms: f64,
    pub factor: f64,
    pub accel_on: bool,
    /// `(speed, gain)` points with speeds going up.
    pub accel_points: Vec<(f64, f64)>,
    pub tail_ms: f64,
}

impl Default for PresetModel {
    fn default() -> Self {
        Self {
            debeat_on: false,
            debeat: DebeatConfig::default(),
            one_euro_on: false,
            min_cutoff_hz: 4.0,
            beta: 0.05,
            d_cutoff_hz: 1.0,
            ema_on: false,
            tau_ms: 4.0,
            spread_on: false,
            spread_ms: 8.75,
            factor: 1.0,
            accel_on: false,
            accel_points: MEDIUM_CURVE.to_vec(),
            tail_ms: PipelineConfig::default().tail_timeout_ms,
        }
    }
}

impl PresetModel {
    pub fn from_config(config: &PipelineConfig) -> Self {
        let mut model = Self {
            factor: config.factor,
            tail_ms: config.tail_timeout_ms,
            ..Self::default()
        };
        for stage in &config.stages {
            match *stage {
                StageConfig::Debeat(debeat) => {
                    model.debeat_on = true;
                    model.debeat = debeat;
                }
                StageConfig::OneEuro {
                    min_cutoff_hz,
                    beta,
                    d_cutoff_hz,
                } => {
                    model.one_euro_on = true;
                    model.min_cutoff_hz = min_cutoff_hz;
                    model.beta = beta;
                    model.d_cutoff_hz = d_cutoff_hz;
                }
                StageConfig::Ema { tau_ms } => {
                    model.ema_on = true;
                    model.tau_ms = tau_ms;
                }
                StageConfig::Spread { window_ms } => {
                    model.spread_on = true;
                    model.spread_ms = window_ms;
                }
            }
        }
        match &config.curve {
            Curve::Flat => {}
            Curve::Table(points) if points.is_empty() => {}
            Curve::Table(points) => {
                model.accel_on = true;
                model.accel_points = points.clone();
            }
            curve => {
                model.accel_on = true;
                model.accel_points = SAMPLE_SPEEDS
                    .iter()
                    .map(|&speed| (speed, curve.gain(speed)))
                    .collect();
            }
        }
        model
    }

    pub fn to_config(&self) -> PipelineConfig {
        let mut stages = Vec::new();
        if self.debeat_on {
            stages.push(StageConfig::Debeat(self.debeat));
        }
        if self.one_euro_on {
            stages.push(StageConfig::OneEuro {
                min_cutoff_hz: self.min_cutoff_hz,
                beta: self.beta,
                d_cutoff_hz: self.d_cutoff_hz,
            });
        }
        if self.ema_on {
            stages.push(StageConfig::Ema {
                tau_ms: self.tau_ms,
            });
        }
        if self.spread_on {
            stages.push(StageConfig::Spread {
                window_ms: self.spread_ms,
            });
        }
        let curve = if self.accel_on {
            Curve::Table(self.accel_points.clone())
        } else {
            Curve::Flat
        };
        PipelineConfig {
            stages,
            factor: self.factor,
            curve,
            tail_timeout_ms: self.tail_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Library;
    use ilumx_core::spec::parse_spec;

    #[test]
    fn default_presets_survive_the_window() {
        let library = Library::default();
        for key in Library::standard_keys() {
            let config = library.config(&key).unwrap();
            let model = PresetModel::from_config(&config);
            assert_eq!(model.to_config(), config, "{key:?}");
        }
    }

    #[test]
    fn tables_survive_the_window() {
        let config = parse_spec("accel:type=table,points=0/1;2/1.5;8/3").unwrap();
        assert_eq!(PresetModel::from_config(&config).to_config(), config);
    }

    #[test]
    fn other_curves_turn_into_points_with_the_same_shape() {
        for text in [
            "accel:type=linear,base=1,slope=0.5,cap=2",
            "accel:type=power,base=1,scale=0.3,exponent=2,cap=4",
        ] {
            let config = parse_spec(text).unwrap();
            let shown = PresetModel::from_config(&config).to_config();
            for speed in SAMPLE_SPEEDS {
                let (want, got) = (config.curve.gain(speed), shown.curve.gain(speed));
                assert!((want - got).abs() < 1e-9, "{text} at {speed}");
            }
        }
    }

    #[test]
    fn a_preset_without_a_curve_keeps_it_off() {
        let model = PresetModel::from_config(&parse_spec("ema:tau_ms=4").unwrap());
        assert!(!model.accel_on);
        assert_eq!(model.to_config().curve, Curve::Flat);
    }

    #[test]
    fn switching_a_part_off_and_on_keeps_its_values() {
        let config = parse_spec("ema:tau_ms=7").unwrap();
        let mut model = PresetModel::from_config(&config);
        model.ema_on = false;
        assert!(model.to_config().stages.is_empty());
        model.ema_on = true;
        assert_eq!(model.to_config(), config);
    }

    #[test]
    fn chains_come_out_with_debeat_first() {
        let model = PresetModel {
            spread_on: true,
            debeat_on: true,
            ..PresetModel::default()
        };
        let stages = model.to_config().stages;
        assert!(matches!(stages[0], StageConfig::Debeat(_)));
        assert!(matches!(stages[1], StageConfig::Spread { .. }));
    }
}
