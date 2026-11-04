//! ilumx-probe: records raw trackball packets and tells which cause of cursor
//! stepping is real. It only observes input and needs no driver.

mod analysis;
mod report;
mod scenarios;

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use ilumx_core::event_log;
use ilumx_win::devices::{self, MouseDevice};
use ilumx_win::rawinput::{Capture, Recording};
use ilumx_win::windows_settings;

use analysis::{analyze, analyze_scenario, Thresholds};
use report::ReportContext;
use scenarios::{Scenario, SCENARIOS};

const DEVICE_FILE: &str = "device.txt";
const REPORT_FILE: &str = "report.md";
const COUNTDOWN_SECONDS: u32 = 3;
/// Below this many packets a recording is probably of the wrong device.
const MIN_MOTION_PACKETS: usize = 50;

#[derive(Parser)]
#[command(version, about = "Диагностика ступенек курсора у трекбола Logitech MX Ergo")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Показать найденные мыши
    List,
    /// Записать сценарии и построить отчёт
    Record(RecordArgs),
    /// Заново построить отчёт по уже записанной папке
    Analyze(AnalyzeArgs),
}

#[derive(Args, Default)]
struct RecordArgs {
    /// Номер устройства из `list`. По умолчанию Logitech
    #[arg(long)]
    device: Option<usize>,
    /// Длина каждого сценария в секундах
    #[arg(long, default_value_t = 10)]
    seconds: u64,
    /// Папка для логов. По умолчанию logs/<дата_время>
    #[arg(long)]
    out: Option<PathBuf>,
    /// Не спрашивать: записать все сценарии подряд
    #[arg(long)]
    no_prompt: bool,
    #[command(flatten)]
    thresholds: ThresholdArgs,
}

#[derive(Args)]
struct AnalyzeArgs {
    /// Папка с логами
    folder: PathBuf,
    #[command(flatten)]
    thresholds: ThresholdArgs,
}

#[derive(Args, Clone, Copy)]
struct ThresholdArgs {
    /// Stick-slip: минимальная пауза перед скачком, мс
    #[arg(long, default_value_t = Thresholds::default().pause_ms)]
    pause_ms: u64,
    /// Stick-slip: минимальный скачок, отсчёты
    #[arg(long, default_value_t = Thresholds::default().jump_counts)]
    jump: f64,
}

impl Default for ThresholdArgs {
    fn default() -> Self {
        let defaults = Thresholds::default();
        Self {
            pause_ms: defaults.pause_ms,
            jump: defaults.jump_counts,
        }
    }
}

impl From<ThresholdArgs> for Thresholds {
    fn from(args: ThresholdArgs) -> Self {
        Self {
            pause_ms: args.pause_ms,
            jump_counts: args.jump,
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::List) => list(),
        Some(Command::Record(args)) => record(&args),
        Some(Command::Analyze(args)) => analyze_folder(&args),
        // Started without arguments, for example by a double click: keep the
        // console open so the result can be read.
        None => {
            let result = record(&RecordArgs {
                seconds: 10,
                ..Default::default()
            });
            finish_interactive_run(result)
        }
    };
    if let Err(error) = result {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}

fn finish_interactive_run(result: Result<()>) -> Result<()> {
    if let Err(error) = &result {
        eprintln!("Ошибка: {error}");
    }
    prompt("\nНажми Enter, чтобы выйти.")?;
    result.map_err(|_| "запись не завершена".into())
}

fn list() -> Result<()> {
    let mice = devices::enumerate()?;
    print_devices(&mice);
    Ok(())
}

fn print_devices(mice: &[MouseDevice]) {
    if mice.is_empty() {
        println!("Мыши не найдены.");
        return;
    }
    let default = devices::default_index(mice);
    println!("Найденные мыши:");
    for (i, mouse) in mice.iter().enumerate() {
        let marker = if Some(i) == default { "*" } else { " " };
        println!("{marker}[{i}] {}", mouse.label());
        println!("     {}", mouse.path);
    }
}

fn record(args: &RecordArgs) -> Result<()> {
    let mice = devices::enumerate()?;
    let device = choose_device(&mice, args)?;
    println!("Записываю только: {}\n", device.label());

    let timestamp = windows_settings::local_timestamp();
    let folder = args
        .out
        .clone()
        .unwrap_or_else(|| Path::new("logs").join(&timestamp));
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join(DEVICE_FILE),
        format!("{}\n{}\n", device.label(), device.path),
    )?;

    let capture = Capture::start()?;
    let duration = Duration::from_secs(args.seconds);
    for (index, scenario) in SCENARIOS.iter().enumerate() {
        println!(
            "Сценарий {}/{}: {}",
            index + 1,
            SCENARIOS.len(),
            scenario.title
        );
        println!("  {}", scenario.hint);
        record_scenario(&capture, device, scenario, duration, &folder, args.no_prompt)?;
        println!();
    }

    let thresholds = args.thresholds.into();
    write_report(&folder, &timestamp, thresholds)
}

/// Offers the MX Ergo by default. Only the chosen device is recorded.
fn choose_device<'a>(mice: &'a [MouseDevice], args: &RecordArgs) -> Result<&'a MouseDevice> {
    if mice.is_empty() {
        return Err("мыши не найдены".into());
    }
    if let Some(index) = args.device {
        return mice
            .get(index)
            .ok_or_else(|| format!("нет устройства с номером {index}").into());
    }

    print_devices(mice);
    let default = devices::default_index(mice);
    if args.no_prompt {
        let index = default.ok_or("Logitech не найден, укажи устройство через --device")?;
        return Ok(&mice[index]);
    }

    loop {
        let question = match default {
            Some(index) => format!("Номер устройства [Enter = {index}]: "),
            None => "Logitech не найден. Номер устройства: ".to_string(),
        };
        let answer = prompt(&question)?;
        let index = match (answer.parse::<usize>(), default) {
            (Ok(index), _) => Some(index),
            (Err(_), Some(index)) if answer.is_empty() => Some(index),
            _ => None,
        };
        if let Some(mouse) = index.and_then(|i| mice.get(i)) {
            return Ok(mouse);
        }
        println!("Нет такого номера.");
    }
}

fn record_scenario(
    capture: &Capture,
    device: &MouseDevice,
    scenario: &Scenario,
    duration: Duration,
    folder: &Path,
    no_prompt: bool,
) -> Result<()> {
    loop {
        if !no_prompt {
            let answer = prompt("  Enter = начать, s = пропустить: ")?;
            if answer.eq_ignore_ascii_case("s") {
                return Ok(());
            }
        }
        countdown();
        let recording = capture.record(device.handle, duration)?;
        println!("  стоп");
        let motion = print_recording_summary(&recording);

        let path = folder.join(format!("{}.csv", scenario.id));
        event_log::write_csv(BufWriter::new(File::create(path)?), &recording.events)?;

        if no_prompt || motion >= MIN_MOTION_PACKETS {
            if no_prompt {
                return Ok(());
            }
            let answer = prompt("  Enter = дальше, r = перезаписать: ")?;
            if !answer.eq_ignore_ascii_case("r") {
                return Ok(());
            }
        } else {
            println!("  Слишком мало данных, запишем ещё раз.");
        }
    }
}

fn print_recording_summary(recording: &Recording) -> usize {
    let motion = recording.events.iter().filter(|e| e.is_motion()).count();
    println!("  Пакетов движения: {motion}");
    if recording.absolute_packets > 0 {
        println!(
            "  Пропущено пакетов с абсолютными координатами: {}",
            recording.absolute_packets
        );
    }
    if motion < MIN_MOTION_PACKETS && recording.other_device_packets > 0 {
        println!(
            "  От других мышей пришло пакетов: {}. Возможно, выбрано не то устройство.",
            recording.other_device_packets
        );
    }
    motion
}

fn countdown() {
    for left in (1..=COUNTDOWN_SECONDS).rev() {
        print!("  {left}... ");
        let _ = io::stdout().flush();
        thread::sleep(Duration::from_secs(1));
    }
    println!("веди!");
}

fn prompt(text: &str) -> io::Result<String> {
    print!("{text}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn analyze_folder(args: &AnalyzeArgs) -> Result<()> {
    let timestamp = windows_settings::local_timestamp();
    write_report(&args.folder, &timestamp, args.thresholds.into())
}

/// Analyses the logs in `folder`, prints the report and saves it there.
fn write_report(folder: &Path, timestamp: &str, thresholds: Thresholds) -> Result<()> {
    let mut analyses = Vec::new();
    for scenario in &SCENARIOS {
        let path = folder.join(format!("{}.csv", scenario.id));
        if path.exists() {
            let events = event_log::read_csv(&path)?;
            analyses.push(analyze_scenario(scenario.id, &events, &thresholds));
        }
    }
    if analyses.is_empty() {
        return Err(format!("в папке {} нет логов", folder.display()).into());
    }

    let device_text = fs::read_to_string(folder.join(DEVICE_FILE)).ok();
    let device = device_text
        .as_deref()
        .and_then(|text| text.lines().next().zip(text.lines().nth(1)));
    let folder_text = folder.display().to_string();
    let context = ReportContext {
        timestamp,
        folder: &folder_text,
        device,
        thresholds,
    };

    // The settings are read now, so a rerun reports the current ones.
    let result = analyze(analyses, windows_settings::read_pointer_settings());
    let text = report::render(&context, &result);
    println!("{text}");

    let path = folder.join(REPORT_FILE);
    fs::write(&path, text)?;
    println!("Отчёт сохранён: {}", path.display());
    Ok(())
}
