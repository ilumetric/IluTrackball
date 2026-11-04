//! Writes the comparison as one self-contained HTML page.

use std::fmt::Write;

use ilumx_core::event_log::RawEvent;
use ilumx_core::metrics::Metrics;


const TEMPLATE: &str = include_str!("report_template.html");
const DATA_MARKER: &str = "/*DATA*/null";

/// One processing variant applied to one log.
pub struct Variant {
    pub spec: String,
    pub output: Vec<RawEvent>,
    pub metrics: Metrics,
}

/// One log with all its variants.
pub struct LogResult {
    pub name: String,
    pub input: Vec<RawEvent>,
    pub variants: Vec<Variant>,
}

pub fn render(title: &str, logs: &[LogResult]) -> String {
    let mut data = String::new();
    let _ = write_data(&mut data, title, logs);
    // Replacing `<` keeps text inside the script block from ending it.
    TEMPLATE.replace(DATA_MARKER, &data.replace('<', "\\u003c"))
}

fn write_data(o: &mut String, title: &str, logs: &[LogResult]) -> std::fmt::Result {
    write!(o, "{{\"title\":{},\"logs\":[", quoted(title))?;
    for (i, log) in logs.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        write!(o, "{{\"name\":{},\"input\":", quoted(&log.name))?;
        write_track(o, &log.input)?;
        o.push_str(",\"variants\":[");
        for (j, variant) in log.variants.iter().enumerate() {
            if j > 0 {
                o.push(',');
            }
            let m = &variant.metrics;
            write!(
                o,
                "{{\"spec\":{},\"lag\":{:.1},\"rough\":{:.3},\"doubles\":{},\"frames\":{:.3},\"maxErr\":{:.1},\"endErr\":{:.1},\"track\":",
                quoted(&variant.spec),
                m.lag_ms,
                m.roughness,
                m.double_steps,
                m.frame_roughness,
                m.max_error_px,
                m.end_error_px
            )?;
            write_track(o, &variant.output)?;
            o.push('}');
        }
        o.push_str("]}");
    }
    o.push_str("]}");
    Ok(())
}

/// Positions as parallel arrays: time in ms and cumulative X and Y.
fn write_track(o: &mut String, events: &[RawEvent]) -> std::fmt::Result {
    let moves: Vec<&RawEvent> = events.iter().filter(|e| e.is_motion()).collect();
    write_series(o, "{\"t\":[", moves.iter().map(|e| e.t_us as f64 / 1000.0), 1)?;
    let mut x = 0;
    write_series(
        o,
        "],\"x\":[",
        moves.iter().map(|e| {
            x += e.dx;
            x as f64
        }),
        0,
    )?;
    let mut y = 0;
    write_series(
        o,
        "],\"y\":[",
        moves.iter().map(|e| {
            y += e.dy;
            y as f64
        }),
        0,
    )?;
    o.push_str("]}");
    Ok(())
}

fn write_series(
    o: &mut String,
    prefix: &str,
    values: impl Iterator<Item = f64>,
    decimals: usize,
) -> std::fmt::Result {
    o.push_str(prefix);
    for (i, value) in values.enumerate() {
        if i > 0 {
            o.push(',');
        }
        write!(o, "{value:.decimals$}")?;
    }
    Ok(())
}

fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(t_us: u64, dx: i32, dy: i32) -> RawEvent {
        RawEvent {
            t_us,
            dx,
            dy,
            ..Default::default()
        }
    }

    fn sample() -> Vec<LogResult> {
        vec![LogResult {
            name: "circle".to_string(),
            input: vec![motion(1_000, 2, 0), motion(2_500, 1, -1)],
            variants: vec![Variant {
                spec: "debeat+ema:tau_ms=5".to_string(),
                output: vec![motion(1_000, 3, -1)],
                metrics: Metrics::default(),
            }],
        }]
    }

    #[test]
    fn embeds_the_data_in_the_template() {
        let page = render("logs/test", &sample());
        assert!(!page.contains(DATA_MARKER));
        assert!(page.contains("\"name\":\"circle\""));
        assert!(page.contains("\"spec\":\"debeat+ema:tau_ms=5\""));
        assert!(page.contains("\"t\":[1.0,2.5],\"x\":[2,3],\"y\":[0,-1]"));
    }

    #[test]
    fn quotes_text_safely() {
        assert_eq!(quoted("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn text_cannot_close_the_script_block() {
        let page = render("</script><b>", &sample());
        assert!(!page.contains("</script><b>"));
    }
}
