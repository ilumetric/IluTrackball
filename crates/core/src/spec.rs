//! Text form of a pipeline, shared by the command line and the presets.
//!
//! Parts are joined with `+`, each with optional `key=value` parameters:
//! `debeat:release_ms=60+ema:tau_ms=5+accel:type=linear,slope=0.1,cap=2`

use std::collections::BTreeMap;

use crate::curve::Curve;
use crate::debeat::DebeatConfig;
use crate::pipeline::{PipelineConfig, StageConfig, DEFAULT_TAIL_TIMEOUT_MS};

type Params<'a> = BTreeMap<&'a str, &'a str>;

pub fn parse_spec(text: &str) -> Result<PipelineConfig, String> {
    let mut config = PipelineConfig::default();
    for part in text.split('+') {
        let (name, params) = part.trim().split_once(':').unwrap_or((part.trim(), ""));
        let mut params = parse_params(params)?;
        match name {
            "passthrough" => {}
            "ema" => config.stages.push(StageConfig::Ema {
                tau_ms: positive(&mut params, "tau_ms", 8.0)?,
            }),
            "one_euro" => config.stages.push(StageConfig::OneEuro {
                min_cutoff_hz: positive(&mut params, "min_cutoff", 2.0)?,
                beta: number(&mut params, "beta", 0.02)?,
                d_cutoff_hz: positive(&mut params, "d_cutoff", 1.0)?,
            }),
            "debeat" => {
                if !config.stages.is_empty() {
                    return Err("debeat must come first: it needs whole packets".to_string());
                }
                let defaults = DebeatConfig::default();
                config.stages.push(StageConfig::Debeat(DebeatConfig {
                    ratio: positive(&mut params, "ratio", defaults.ratio)?,
                    window_ratio: positive(&mut params, "window_ratio", defaults.window_ratio)?,
                    max_ratio: positive(&mut params, "max_ratio", defaults.max_ratio)?,
                    release_s: positive(&mut params, "release_ms", defaults.release_s * 1000.0)?
                        / 1000.0,
                    period_s: positive(&mut params, "period_ms", defaults.period_s * 1000.0)?
                        / 1000.0,
                }));
            }
            "spread" => config.stages.push(StageConfig::Spread {
                window_ms: positive(&mut params, "ms", 8.75)?,
            }),
            "precision" => config.factor *= positive(&mut params, "factor", 0.5)?,
            "accel" => config.curve = parse_curve(&mut params)?,
            "tail" => config.tail_timeout_ms = positive(&mut params, "ms", config.tail_timeout_ms)?,
            other => return Err(format!("unknown mode `{other}`")),
        }
        if let Some(key) = params.keys().next() {
            return Err(format!("unknown parameter `{key}` for `{name}`"));
        }
    }
    Ok(config)
}

/// Writes a pipeline back in its text form, leaving out default values.
pub fn format_spec(config: &PipelineConfig) -> String {
    let mut parts: Vec<String> = config.stages.iter().map(format_stage).collect();
    if config.factor != 1.0 {
        parts.push(format!("precision:factor={}", tidy(config.factor)));
    }
    if config.curve != Curve::Flat {
        parts.push(format_curve(&config.curve));
    }
    if config.tail_timeout_ms != DEFAULT_TAIL_TIMEOUT_MS {
        parts.push(format!("tail:ms={}", tidy(config.tail_timeout_ms)));
    }
    if parts.is_empty() {
        return "passthrough".to_string();
    }
    parts.join("+")
}

fn format_stage(stage: &StageConfig) -> String {
    match stage {
        StageConfig::Ema { tau_ms } => format!("ema:tau_ms={}", tidy(*tau_ms)),
        StageConfig::OneEuro {
            min_cutoff_hz,
            beta,
            d_cutoff_hz,
        } => format!(
            "one_euro:min_cutoff={},beta={},d_cutoff={}",
            tidy(*min_cutoff_hz),
            tidy(*beta),
            tidy(*d_cutoff_hz)
        ),
        StageConfig::Debeat(debeat) => format_debeat(debeat),
        StageConfig::Spread { window_ms } => format!("spread:ms={}", tidy(*window_ms)),
    }
}

fn format_debeat(debeat: &DebeatConfig) -> String {
    let defaults = DebeatConfig::default();
    let mut params = Vec::new();
    let mut add = |name: &str, value: f64, default: f64| {
        if tidy(value) != tidy(default) {
            params.push(format!("{name}={}", tidy(value)));
        }
    };
    add("ratio", debeat.ratio, defaults.ratio);
    add("window_ratio", debeat.window_ratio, defaults.window_ratio);
    add("max_ratio", debeat.max_ratio, defaults.max_ratio);
    add("release_ms", debeat.release_s * 1000.0, defaults.release_s * 1000.0);
    add("period_ms", debeat.period_s * 1000.0, defaults.period_s * 1000.0);
    if params.is_empty() {
        "debeat".to_string()
    } else {
        format!("debeat:{}", params.join(","))
    }
}

fn format_curve(curve: &Curve) -> String {
    match curve {
        Curve::Flat => String::new(),
        Curve::Linear { base, slope, cap } => format!(
            "accel:type=linear,base={},slope={},cap={}",
            tidy(*base),
            tidy(*slope),
            tidy(*cap)
        ),
        Curve::Power {
            base,
            scale,
            exponent,
            cap,
        } => format!(
            "accel:type=power,base={},scale={},exponent={},cap={}",
            tidy(*base),
            tidy(*scale),
            tidy(*exponent),
            tidy(*cap)
        ),
        Curve::Table(points) => {
            let points: Vec<String> = points
                .iter()
                .map(|(speed, gain)| format!("{}/{}", tidy(*speed), tidy(*gain)))
                .collect();
            format!("accel:type=table,points={}", points.join(";"))
        }
    }
}

/// Drops the floating point noise left by unit conversions.
fn tidy(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

fn parse_params(text: &str) -> Result<Params<'_>, String> {
    let mut params = Params::new();
    for pair in text.split(',').filter(|pair| !pair.trim().is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("expected key=value, found `{pair}`"))?;
        params.insert(key.trim(), value.trim());
    }
    Ok(params)
}

fn parse_curve(params: &mut Params) -> Result<Curve, String> {
    let kind = params.remove("type").unwrap_or("linear");
    match kind {
        "linear" => Ok(Curve::Linear {
            base: number(params, "base", 1.0)?,
            slope: number(params, "slope", 0.1)?,
            cap: positive(params, "cap", 3.0)?,
        }),
        "power" => Ok(Curve::Power {
            base: number(params, "base", 1.0)?,
            scale: positive(params, "scale", 0.2)?,
            exponent: positive(params, "exponent", 1.5)?,
            cap: positive(params, "cap", 3.0)?,
        }),
        "table" => {
            let points = params
                .remove("points")
                .ok_or("accel type=table needs points=speed/gain;...")?;
            Ok(Curve::Table(parse_points(points)?))
        }
        other => Err(format!("unknown accel type `{other}`")),
    }
}

fn parse_points(text: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut points = Vec::new();
    for pair in text.split(';') {
        let (speed, gain) = pair
            .split_once('/')
            .ok_or_else(|| format!("expected speed/gain, found `{pair}`"))?;
        points.push((parse_number("points", speed)?, parse_number("points", gain)?));
    }
    if points.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err("table points must go up in speed".to_string());
    }
    if points.iter().any(|point| point.1 <= 0.0) {
        return Err("table gains must be above zero".to_string());
    }
    Ok(points)
}

fn number(params: &mut Params, key: &str, default: f64) -> Result<f64, String> {
    params
        .remove(key)
        .map_or(Ok(default), |value| parse_number(key, value))
}

fn positive(params: &mut Params, key: &str, default: f64) -> Result<f64, String> {
    let value = number(params, key, default)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err(format!("{key} must be above zero"))
    }
}

fn parse_number(key: &str, value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(number) if number.is_finite() => Ok(number),
        _ => Err(format!("{key} is not a number: `{value}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_is_the_default_pipeline() {
        assert_eq!(parse_spec("passthrough").unwrap(), PipelineConfig::default());
    }

    #[test]
    fn parts_chain_in_order() {
        let config = parse_spec("debeat+ema:tau_ms=5").unwrap();
        assert_eq!(
            config.stages,
            [
                StageConfig::Debeat(DebeatConfig::default()),
                StageConfig::Ema { tau_ms: 5.0 }
            ]
        );
    }

    #[test]
    fn reads_all_parameters() {
        let config = parse_spec(
            "one_euro:min_cutoff=3,beta=0.05,d_cutoff=2+precision:factor=0.25+tail:ms=80",
        )
        .unwrap();
        assert_eq!(
            config.stages,
            [StageConfig::OneEuro {
                min_cutoff_hz: 3.0,
                beta: 0.05,
                d_cutoff_hz: 2.0
            }]
        );
        assert_eq!(config.factor, 0.25);
        assert_eq!(config.tail_timeout_ms, 80.0);
    }

    #[test]
    fn reads_debeat_release_in_milliseconds() {
        let config = parse_spec("debeat:release_ms=100").unwrap();
        let StageConfig::Debeat(debeat) = config.stages[0] else {
            panic!("not debeat");
        };
        assert!((debeat.release_s - 0.1).abs() < 1e-12);
    }

    #[test]
    fn reads_every_curve_type() {
        let linear = parse_spec("accel:type=linear,base=1,slope=0.5,cap=2").unwrap();
        assert_eq!(
            linear.curve,
            Curve::Linear {
                base: 1.0,
                slope: 0.5,
                cap: 2.0
            }
        );
        let power = parse_spec("accel:type=power,scale=0.5,exponent=2").unwrap();
        assert!(matches!(power.curve, Curve::Power { exponent, .. } if exponent == 2.0));
        let table = parse_spec("accel:type=table,points=0/1;2/1.5;8/3").unwrap();
        assert_eq!(
            table.curve,
            Curve::Table(vec![(0.0, 1.0), (2.0, 1.5), (8.0, 3.0)])
        );
    }

    #[test]
    fn text_form_survives_a_round_trip() {
        for text in [
            "passthrough",
            "debeat",
            "debeat:release_ms=30+ema:tau_ms=4",
            "debeat:ratio=1.9,window_ratio=1.2,max_ratio=4,release_ms=25,period_ms=99+spread:ms=16.7",
            "one_euro:min_cutoff=4,beta=0.05,d_cutoff=1.5",
            "ema:tau_ms=8+precision:factor=0.25",
            "accel:type=linear,base=1,slope=0.1,cap=2",
            "accel:type=power,base=1,scale=0.2,exponent=1.5,cap=3",
            "accel:type=table,points=0/1;2/1.5;8/3",
            "ema:tau_ms=8+tail:ms=80",
        ] {
            let config = parse_spec(text).unwrap();
            assert_eq!(format_spec(&config), text);
            assert_eq!(parse_spec(&format_spec(&config)).unwrap(), config);
        }
    }

    #[test]
    fn rejects_mistakes_with_a_clear_message() {
        let error = |text| parse_spec(text).unwrap_err();
        assert!(error("smooth").contains("unknown mode"));
        assert!(error("ema:tau=5").contains("unknown parameter"));
        assert!(error("ema:tau_ms=fast").contains("not a number"));
        assert!(error("ema:tau_ms=0").contains("above zero"));
        assert!(error("ema:tau_ms").contains("key=value"));
        assert!(error("ema+debeat").contains("must come first"));
        assert!(error("accel:type=magic").contains("unknown accel type"));
        assert!(error("accel:type=table").contains("needs points"));
        assert!(error("accel:type=table,points=2/1;1/2").contains("go up"));
        assert!(error("accel:type=table,points=0/0;1/2").contains("above zero"));
    }
}
