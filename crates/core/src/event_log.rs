//! The CSV event log written by `ilumx-probe` and consumed by the other tools.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Header line of every event log.
pub const CSV_HEADER: &str = "t_qpc_us,dx,dy,buttons,wheel";

/// One raw input packet from the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawEvent {
    /// Microseconds since the start of the recording, from the QPC clock.
    pub t_us: u64,
    pub dx: i32,
    pub dy: i32,
    /// Bitmask of held buttons: left 1, right 2, middle 4, X1 8, X2 16.
    pub buttons: u32,
    /// Signed vertical wheel delta of this packet.
    pub wheel: i32,
}

impl RawEvent {
    pub fn is_motion(&self) -> bool {
        self.dx != 0 || self.dy != 0
    }
}

/// Writes a complete log, header included.
pub fn write_csv<W: Write>(mut out: W, events: &[RawEvent]) -> io::Result<()> {
    writeln!(out, "{CSV_HEADER}")?;
    for e in events {
        writeln!(
            out,
            "{},{},{},{},{}",
            e.t_us, e.dx, e.dy, e.buttons, e.wheel
        )?;
    }
    out.flush()
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number in the source text.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl Error for ParseError {}

/// Parses a log from text. Blank lines are skipped.
pub fn parse_csv(text: &str) -> Result<Vec<RawEvent>, ParseError> {
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, header)) if header.trim() == CSV_HEADER => {}
        _ => {
            return Err(ParseError {
                line: 1,
                message: format!("expected header `{CSV_HEADER}`"),
            })
        }
    }

    let mut events = Vec::new();
    for (index, line) in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        events.push(parse_row(line).map_err(|message| ParseError {
            line: index + 1,
            message,
        })?);
    }
    Ok(events)
}

fn parse_row(line: &str) -> Result<RawEvent, String> {
    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    let [t_us, dx, dy, buttons, wheel] = fields[..] else {
        return Err(format!("expected 5 fields, found {}", fields.len()));
    };
    Ok(RawEvent {
        t_us: parse_field(t_us, "t_qpc_us")?,
        dx: parse_field(dx, "dx")?,
        dy: parse_field(dy, "dy")?,
        buttons: parse_field(buttons, "buttons")?,
        wheel: parse_field(wheel, "wheel")?,
    })
}

fn parse_field<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name} value `{value}`"))
}

/// Reads and parses a log file.
pub fn read_csv(path: &Path) -> io::Result<Vec<RawEvent>> {
    let text = fs::read_to_string(path)?;
    parse_csv(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<RawEvent> {
        vec![
            RawEvent {
                t_us: 0,
                dx: 1,
                dy: -1,
                buttons: 0,
                wheel: 0,
            },
            RawEvent {
                t_us: 8000,
                dx: 0,
                dy: 0,
                buttons: 5,
                wheel: -120,
            },
        ]
    }

    #[test]
    fn round_trips_through_csv() {
        let mut bytes = Vec::new();
        write_csv(&mut bytes, &sample()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with(CSV_HEADER));
        assert_eq!(parse_csv(&text).unwrap(), sample());
    }

    #[test]
    fn accepts_crlf_and_blank_lines() {
        let text = format!("{CSV_HEADER}\r\n0,1,2,0,0\r\n\r\n8,3,4,0,0\r\n");
        assert_eq!(parse_csv(&text).unwrap().len(), 2);
    }

    #[test]
    fn rejects_a_wrong_header() {
        assert_eq!(parse_csv("t,dx\n").unwrap_err().line, 1);
    }

    #[test]
    fn reports_the_line_of_a_bad_row() {
        let text = format!("{CSV_HEADER}\n0,1,2,0,0\n8,x,4,0,0\n");
        let error = parse_csv(&text).unwrap_err();
        assert_eq!(error.line, 3);
        assert!(error.message.contains("dx"));
    }

    #[test]
    fn rejects_a_short_row() {
        let text = format!("{CSV_HEADER}\n0,1,2\n");
        assert_eq!(parse_csv(&text).unwrap_err().line, 2);
    }

    #[test]
    fn motion_needs_a_nonzero_delta() {
        let [moved, still] = sample()[..] else {
            unreachable!()
        };
        assert!(moved.is_motion());
        assert!(!still.is_motion());
    }
}
