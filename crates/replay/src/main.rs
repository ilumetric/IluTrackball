//! ilumx-replay: runs recorded logs through processing modes offline, so
//! parameters can be tuned without touching live input.

mod html;

use std::error::Error;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use clap::Parser;
use ilumx_core::event_log;
use ilumx_core::metrics::measure;
use ilumx_core::pipeline::PipelineConfig;
use ilumx_core::simulate::simulate;
use ilumx_core::spec::parse_spec;

use html::{LogResult, Variant};

const REPORT_FILE: &str = "report.html";
const OUTPUT_FOLDER: &str = "replay";

/// Modes compared when none are given: the reference, the best debeat
/// variants for each amount of lag, and plain smoothing to compare them with.
const DEFAULT_SPECS: [&str; 7] = [
    "passthrough",
    "debeat:release_ms=40",
    "debeat:release_ms=30+ema:tau_ms=4",
    "debeat:release_ms=60+spread:ms=16.7",
    "ema:tau_ms=8",
    "ema:tau_ms=16",
    "one_euro:min_cutoff=4,beta=0.05",
];

#[derive(Parser)]
#[command(version, about = "Прогон записанных логов через режимы фильтра")]
struct Cli {
    /// CSV из ilumx-probe или папка с такими файлами
    input: PathBuf,
    /// Режим, можно указать несколько раз. Пример: debeat:release_ms=60+ema:tau_ms=4
    #[arg(long)]
    spec: Vec<String>,
    /// Куда писать результат. По умолчанию папка replay рядом с логами
    #[arg(long)]
    out: Option<PathBuf>,
    /// Частота монитора, Гц: на ней считается неровность по кадрам
    #[arg(long, default_value_t = 60.0)]
    refresh: f64,
}

fn main() {
    if let Err(error) = run(&Cli::parse()) {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let specs = chosen_specs(cli)?;
    let files = log_files(&cli.input)?;
    if files.is_empty() {
        return Err(format!("в {} нет CSV", cli.input.display()).into());
    }
    let out = cli.out.clone().unwrap_or_else(|| default_output(&cli.input));
    fs::create_dir_all(&out)?;

    let mut logs = Vec::new();
    for file in &files {
        let name = file
            .file_stem()
            .map_or_else(|| "log".to_string(), |s| s.to_string_lossy().into_owned());
        let input = event_log::read_csv(file)?;

        let mut variants = Vec::new();
        for (spec, config) in &specs {
            let output = simulate(&input, config);
            let metrics = measure(&input, &output, cli.refresh);
            let path = out.join(format!("{name}.{}.csv", file_slug(spec)));
            event_log::write_csv(BufWriter::new(File::create(path)?), &output)?;
            variants.push(Variant {
                spec: spec.clone(),
                output,
                metrics,
            });
        }
        logs.push(LogResult {
            name,
            input,
            variants,
        });
    }

    print_summary(&logs);
    let page = out.join(REPORT_FILE);
    fs::write(&page, html::render(&cli.input.display().to_string(), &logs))?;
    println!("Графики: {}", page.display());
    Ok(())
}

fn chosen_specs(cli: &Cli) -> Result<Vec<(String, PipelineConfig)>, Box<dyn Error>> {
    let texts: Vec<String> = if cli.spec.is_empty() {
        DEFAULT_SPECS.iter().map(ToString::to_string).collect()
    } else {
        cli.spec.clone()
    };
    texts
        .into_iter()
        .map(|text| {
            let config = parse_spec(&text).map_err(|error| format!("режим `{text}`: {error}"))?;
            Ok((text, config))
        })
        .collect()
}

fn log_files(input: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if !input.is_dir() {
        return Ok(vec![input.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(input)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "csv") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn default_output(input: &Path) -> PathBuf {
    if input.is_dir() {
        input.join(OUTPUT_FOLDER)
    } else {
        input
            .parent()
            .unwrap_or(Path::new("."))
            .join(OUTPUT_FOLDER)
    }
}

/// A spec as a file name: everything but letters, digits and dots becomes `_`.
fn file_slug(spec: &str) -> String {
    spec.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn print_summary(logs: &[LogResult]) {
    for log in logs {
        println!("{}", log.name);
        println!(
            "  {:<34} {:>7} {:>11} {:>8} {:>7} {:>11}",
            "режим", "лаг, мс", "неровность", "двойные", "кадры", "отставание"
        );
        for variant in &log.variants {
            let m = &variant.metrics;
            println!(
                "  {:<34} {:>7.1} {:>11.3} {:>8} {:>7.3} {:>11.1}",
                variant.spec,
                m.lag_ms,
                m.roughness,
                m.double_steps,
                m.frame_roughness,
                m.max_error_px
            );
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(file_slug("debeat+ema:tau_ms=4.5"), "debeat_ema_tau_ms_4.5");
    }

    #[test]
    fn output_goes_next_to_the_logs() {
        assert_eq!(
            default_output(Path::new("logs/a/circle.csv")),
            Path::new("logs/a/replay")
        );
    }

    #[test]
    fn default_specs_are_all_valid() {
        for spec in DEFAULT_SPECS {
            parse_spec(spec).unwrap();
        }
    }

    #[test]
    fn a_bad_spec_is_reported_by_name() {
        let cli = Cli {
            input: PathBuf::from("x.csv"),
            spec: vec!["smooth".to_string()],
            out: None,
            refresh: 60.0,
        };
        let error = chosen_specs(&cli).unwrap_err().to_string();
        assert!(error.contains("smooth"));
    }
}
