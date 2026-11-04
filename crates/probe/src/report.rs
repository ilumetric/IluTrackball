//! Renders the analysis as a Markdown report in Russian.

use std::fmt::Write;

use ilumx_core::device::{classify_connection, Connection};

use crate::analysis::{
    Analysis, AxisHistogram, Cause, Finding, Level, PointerSettings, Thresholds,
};

/// Examples of stick-slip events printed in the report.
const MAX_LISTED_STICK_SLIPS: usize = 8;

pub struct ReportContext<'a> {
    pub timestamp: &'a str,
    pub folder: &'a str,
    /// Device label and path, if known.
    pub device: Option<(&'a str, &'a str)>,
    pub thresholds: Thresholds,
}

pub fn render(context: &ReportContext, analysis: &Analysis) -> String {
    let mut out = String::new();
    let o = &mut out;
    let _ = render_into(o, context, analysis);
    out
}

fn render_into(o: &mut String, context: &ReportContext, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "# Отчёт ilumx-probe\n")?;
    writeln!(o, "- Время: {}", context.timestamp)?;
    writeln!(o, "- Логи: `{}`", context.folder)?;
    match context.device {
        Some((label, _)) => writeln!(o, "- Устройство: {label}\n")?,
        None => writeln!(o, "- Устройство: не указано\n")?,
    }

    render_verdict(o, a)?;
    render_settings(o, a)?;
    render_scenarios(o, a)?;
    render_double_frames(o, a)?;
    render_polling(o, a)?;
    render_quantization(o, a)?;
    render_stick_slip(o, context, a)?;
    render_advice(o, context, a)
}

fn render_verdict(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Итог\n")?;
    match a.primary() {
        Some(primary) => writeln!(
            o,
            "**Наиболее вероятная причина: {}** ({}).\n",
            cause_name(primary.cause),
            level_name(primary.level)
        )?,
        None => writeln!(o, "**Явной причины ступенек в этих логах не видно.**\n")?,
    }
    writeln!(o, "| Причина | Оценка | Вывод |")?;
    writeln!(o, "|---|---:|---|")?;
    for f in &a.findings {
        writeln!(
            o,
            "| {} | {:.2} | {} |",
            cause_name(f.cause),
            f.score,
            level_name(f.level)
        )?;
    }
    writeln!(o, "\nОценка 1.00 и выше означает явную проблему.\n")
}

fn render_settings(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Настройки Windows\n")?;
    match (a.settings.speed, a.settings.multiplier()) {
        (Some(speed), Some(multiplier)) => {
            writeln!(
                o,
                "- Скорость указателя: {speed} из 20, 1 отсчёт = {multiplier} px"
            )?;
            if speed > 10 {
                writeln!(
                    o,
                    "- **Внимание:** скорость выше 10. Каждый отсчёт сенсора превращается \
                     в прыжок больше одного пикселя."
                )?;
            }
        }
        _ => writeln!(o, "- Скорость указателя: прочитать не удалось")?,
    }
    match a.settings.enhanced_precision {
        Some(true) => writeln!(
            o,
            "- Повышенная точность указателя: **включена**. Это ускорение, оно делает \
             шаг курсора нелинейным."
        )?,
        Some(false) => writeln!(o, "- Повышенная точность указателя: выключена")?,
        None => writeln!(o, "- Повышенная точность указателя: прочитать не удалось")?,
    }
    writeln!(o)
}

fn render_scenarios(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Сценарии\n")?;
    writeln!(
        o,
        "| Сценарий | Длина, с | Пакеты движения | Медленные пакеты | Быстрые интервалы | Stick-slip |"
    )?;
    writeln!(o, "|---|---:|---:|---:|---:|---:|")?;
    for s in &a.scenarios {
        writeln!(
            o,
            "| {} | {:.1} | {} | {} | {} | {} |",
            s.name,
            s.duration_s,
            s.motion_packets,
            s.slow_packets,
            s.fast_intervals_us.len(),
            s.stick_slips.len()
        )?;
    }
    writeln!(o)
}

fn render_double_frames(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Двойные кадры\n")?;
    writeln!(
        o,
        "Интервалы между пакетами могут быть идеальными, а размер пакетов нет: если \
         сенсор и связь тикают на разных частотах, в один пакет иногда попадает два \
         кадра сенсора, и курсор прыгает на двойной шаг.\n"
    )?;
    let Some(d) = &a.double_frames else {
        return writeln!(o, "Всплесков слишком мало, ритма в них не видно.\n");
    };
    writeln!(
        o,
        "- Всплесков: {} (1 на {:.0} пакетов)",
        d.spikes,
        d.checked_packets as f64 / d.spikes as f64
    )?;
    writeln!(
        o,
        "- Типичный всплеск: в {:.2} раза больше соседних пакетов",
        d.median_ratio
    )?;
    writeln!(
        o,
        "- Ритм: каждые {:.1} пакета, это {:.0} мс или {:.1} раза в секунду",
        d.period_packets,
        d.period_ms(),
        d.rate_hz()
    )?;
    writeln!(
        o,
        "- В этот ритм попадает {:.0}% расстояний между всплесками",
        d.regular_share * 100.0
    )?;
    writeln!(
        o,
        "- Связь: {:.1} Гц. Если причина в биениях, сенсор работает примерно на {:.0} Гц\n",
        d.link_hz(),
        d.link_hz() + d.rate_hz()
    )
}

fn render_polling(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Опрос\n")?;
    writeln!(
        o,
        "Считается только там, где шарик крутится быстро: у сенсора есть данные на \
         каждый пакет, и пропуск может дать только связь.\n"
    )?;
    let Some(t) = &a.timing else {
        return writeln!(
            o,
            "Недостаточно быстрых движений. Перезапиши сценарий `fast`.\n"
        );
    };
    writeln!(o, "- Интервалов: {}", t.intervals)?;
    writeln!(
        o,
        "- Медиана: {:.1} мс (частота при движении {:.0} Гц)",
        t.nominal_us as f64 / 1000.0,
        t.rate_hz
    )?;
    writeln!(
        o,
        "- p95: {:.1} мс, p99: {:.1} мс",
        t.p95_us as f64 / 1000.0,
        t.p99_us as f64 / 1000.0
    )?;
    writeln!(o, "- Джиттер (σ): {:.1} мс", t.jitter_us / 1000.0)?;
    writeln!(
        o,
        "- Пропуски длиннее 2× медианы: {:.1}%",
        t.miss_share * 100.0
    )?;
    writeln!(
        o,
        "- Пакеты, пришедшие пачкой: {:.1}%",
        t.bunched_share * 100.0
    )?;
    if let Some(pauses) = a.link_pauses_per_10s {
        writeln!(o, "- Паузы на ходу: {pauses:.1} на 10 с")?;
    }
    writeln!(o)
}

fn render_quantization(o: &mut String, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Квантование\n")?;
    let Some(share) = a.slow_unit_share else {
        return writeln!(
            o,
            "Недостаточно медленных движений. Перезапиши сценарии `slow_h` и `slow_v`.\n"
        );
    };
    writeln!(o, "Медленных пакетов: {}.\n", a.slow_packets)?;
    writeln!(o, "| Ось | 0 | ±1 | ±2 | ±3 и больше |")?;
    writeln!(o, "|---|---:|---:|---:|---:|")?;
    write_histogram_row(o, "X", &a.slow_dx)?;
    write_histogram_row(o, "Y", &a.slow_dy)?;
    writeln!(
        o,
        "\nПакетов с шагом ровно в 1 отсчёт: {:.0}%.",
        share * 100.0
    )?;
    if let Some(cps) = a.slow_median_cps {
        writeln!(o, "Типичная скорость на медленных участках: {cps:.0} отсч/с.")?;
    }
    writeln!(o)
}

fn write_histogram_row(o: &mut String, axis: &str, h: &AxisHistogram) -> std::fmt::Result {
    let percent = |n: usize| n as f64 * 100.0 / h.total().max(1) as f64;
    writeln!(
        o,
        "| {axis} | {:.0}% | {:.0}% | {:.0}% | {:.0}% |",
        percent(h.zero),
        percent(h.one),
        percent(h.two),
        percent(h.three_plus)
    )
}

fn render_stick_slip(o: &mut String, context: &ReportContext, a: &Analysis) -> std::fmt::Result {
    let thresholds = &context.thresholds;
    writeln!(o, "## Stick-slip\n")?;
    writeln!(
        o,
        "Паттерн: пауза от {} мс, затем скачок от {} отсчётов в ту же сторону.\n",
        thresholds.pause_ms, thresholds.jump_counts
    )?;
    let Some(rate) = a.stick_slips_per_10s else {
        return writeln!(
            o,
            "Недостаточно медленных движений. Перезапиши сценарии `slow_h` и `slow_v`.\n"
        );
    };
    writeln!(
        o,
        "- Частота: {rate:.1} на 10 с медленного движения ({:.1} с в логах)",
        a.slow_active_s
    )?;

    match a.link_pauses_per_10s {
        Some(link) if link > 0.0 => writeln!(
            o,
            "- Такие же паузы на быстрых движениях: {link:.1} на 10 с. Это доля связи, \
             она вычтена из оценки: обрыв при медленном движении выглядит так же."
        )?,
        Some(_) => writeln!(o, "- На быстрых движениях таких пауз нет: связь ни при чём.")?,
        None => writeln!(
            o,
            "- Быстрых движений мало, поэтому паузы нельзя отделить от обрывов связи."
        )?,
    }

    let mut listed = 0;
    for scenario in &a.scenarios {
        for event in &scenario.stick_slips {
            if listed == MAX_LISTED_STICK_SLIPS {
                return writeln!(o);
            }
            writeln!(
                o,
                "- `{}` на {:.2} с: пауза {:.0} мс, скачок {:.0} отсч.",
                scenario.name,
                event.t_us as f64 / 1e6,
                event.pause_ms,
                event.jump_counts
            )?;
            listed += 1;
        }
    }
    writeln!(o)
}

fn render_advice(o: &mut String, context: &ReportContext, a: &Analysis) -> std::fmt::Result {
    writeln!(o, "## Что сделать\n")?;
    let active: Vec<&Finding> = a
        .findings
        .iter()
        .filter(|f| f.level > Level::None)
        .collect();
    if active.is_empty() {
        writeln!(
            o,
            "Менять настройки по этим логам незачем. Если ступеньки всё равно видны, \
             переходи к подбору фильтра в ilumx-replay."
        )?;
    }
    for finding in active {
        writeln!(o, "**{}**\n", cause_name(finding.cause))?;
        for line in advice(finding.cause, context, &a.settings) {
            writeln!(o, "- {line}")?;
        }
        writeln!(o)?;
    }
    Ok(())
}

fn advice(
    cause: Cause,
    context: &ReportContext,
    settings: &PointerSettings,
) -> Vec<&'static str> {
    let link = context
        .device
        .map(|(_, path)| classify_connection(path))
        .unwrap_or(Connection::Other);
    match cause {
        Cause::DoubleFrames => {
            let mut lines = Vec::new();
            if link == Connection::Bluetooth {
                lines.push(
                    "Если есть ресивер Unifying, подключи трекбол через него и запиши логи \
                     заново: так проверим, виноват ли Bluetooth. Ресивер Bolt с MX Ergo B01D \
                     не спаривается.",
                );
            }
            if settings.enhanced_precision != Some(false) {
                lines.push(
                    "Выключи «Повышенную точность указателя»: она усиливает крупные пакеты, \
                     а значит и двойной шаг.",
                );
            }
            lines.push("Двойной шаг можно убрать программно: режим debeat в ilumx-replay.");
            lines
        }
        Cause::Quantization => {
            let mut lines = Vec::new();
            if settings.speed != Some(10) {
                lines.push(
                    "Поставь скорость указателя Windows на 10 из 20: тогда 1 отсчёт = 1 пиксель.",
                );
            }
            if settings.enhanced_precision != Some(false) {
                lines.push("Выключи «Повышенную точность указателя».");
            }
            lines.push(
                "Подними DPI трекбола в Options+ или OpenLogi, чтобы вернуть нужную скорость курсора.",
            );
            lines
        }
        Cause::StickSlip => vec![
            "Вынь шарик и почисти опоры в гнезде: на них копится налёт с пальца.",
            "Протри сам шарик сухой безворсовой салфеткой.",
            "Сгладить остаток сможет фильтр one_euro, но механику он не вылечит.",
        ],
        Cause::Polling => match link {
            Connection::Bluetooth => vec![
                "Подключи трекбол через ресивер Unifying вместо Bluetooth. \
                 Ресивер Bolt с MX Ergo B01D не спаривается.",
                "Проверь заряд батареи и убери источники помех 2.4 ГГц.",
            ],
            _ => vec![
                "Переставь ресивер ближе к трекболу, лучше на USB-удлинитель.",
                "Убери ресивер от портов USB 3.0 и роутера: они шумят в диапазоне 2.4 ГГц.",
                "Проверь заряд батареи.",
            ],
        },
    }
}

fn cause_name(cause: Cause) -> &'static str {
    match cause {
        Cause::DoubleFrames => "двойные кадры (биения сенсора и связи)",
        Cause::Quantization => "квантование",
        Cause::StickSlip => "stick-slip (залипание шарика)",
        Cause::Polling => "неравномерный опрос",
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::None => "не подтверждается",
        Level::Weak => "слабые признаки",
        Level::Likely => "вероятно",
        Level::Strong => "явно",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::test_support::{session, unit_speed, Model};
    use crate::analysis::Analysis;

    const BLUETOOTH: &str = r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d_PID&b01d";
    const RECEIVER: &str = r"\\?\HID#VID_046D&PID_C52B&MI_01";

    fn context<'a>(path: &'a str) -> ReportContext<'a> {
        ReportContext {
            timestamp: "2026-11-04_19-52-10",
            folder: "logs/test",
            device: Some(("MX Ergo", path)),
            thresholds: Thresholds::default(),
        }
    }

    fn polling_session() -> Analysis {
        let lossy = |cps| Model {
            link_loss: Some((0.08, 2)),
            ..Model::new(10.0, cps)
        };
        session(&lossy(100.0), &lossy(1500.0), unit_speed())
    }

    #[test]
    fn names_the_primary_cause_and_warns_about_speed() {
        let settings = PointerSettings {
            speed: Some(16),
            enhanced_precision: Some(true),
        };
        let analysis = session(&Model::new(10.0, 100.0), &Model::new(10.0, 1500.0), settings);
        let text = render(&context(RECEIVER), &analysis);
        assert!(text.contains("Наиболее вероятная причина: квантование"));
        assert!(text.contains("скорость выше 10"));
        assert!(text.contains("**включена**"));
        assert!(text.contains("DPI трекбола"));
    }

    #[test]
    fn explains_double_frames_with_their_rhythm() {
        let beating = |cps| Model {
            poll_us: 8_750,
            frame_us: Some(8_060),
            ..Model::new(10.0, cps)
        };
        let analysis = session(&beating(350.0), &beating(1500.0), unit_speed());
        let text = render(&context(BLUETOOTH), &analysis);
        assert!(text.contains("Наиболее вероятная причина: двойные кадры"));
        assert!(text.contains("раза в секунду"));
        assert!(text.contains("ресивер Unifying"));
        assert!(text.contains("режим debeat"));
    }

    #[test]
    fn does_not_advise_what_is_already_set() {
        let settings = PointerSettings {
            speed: Some(10),
            enhanced_precision: Some(false),
        };
        let lines = advice(Cause::Quantization, &context(RECEIVER), &settings);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("DPI трекбола"));
    }

    #[test]
    fn polling_advice_depends_on_the_link() {
        let analysis = polling_session();
        let bluetooth = render(&context(BLUETOOTH), &analysis);
        assert!(bluetooth.contains("вместо Bluetooth"));
        let receiver = render(&context(RECEIVER), &analysis);
        assert!(receiver.contains("USB-удлинитель"));
    }

    #[test]
    fn says_so_when_data_is_missing() {
        let analysis = session(&Model::new(0.2, 100.0), &Model::new(0.1, 1500.0), unit_speed());
        let text = render(&context(RECEIVER), &analysis);
        assert!(text.contains("Явной причины ступенек"));
        assert!(text.contains("Перезапиши сценарий `fast`"));
    }

    #[test]
    fn caps_the_list_of_stick_slip_examples() {
        let sticky = Model {
            stick_us: Some((200_000, 80_000)),
            ..Model::new(10.0, 100.0)
        };
        let analysis = session(&sticky, &Model::new(10.0, 1500.0), unit_speed());
        let text = render(&context(RECEIVER), &analysis);
        let examples = text.matches(" с: пауза ").count();
        assert_eq!(examples, MAX_LISTED_STICK_SLIPS);
    }
}
