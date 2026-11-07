//! Командная строка: накачка, встраивание, извлечение и проверка без GUI.

use std::io::Write;
use std::path::PathBuf;

use crate::payload::{self, ContainerOptions};
use crate::pumper::{AnimationFormat, Method};
use crate::util::{Job, MIB};

const HELP: &str = "\
FilePumper

Использование:
  file-pumper                     запустить графический интерфейс (без консоли)
  file-pumper-cli                 эта справка
  file-pumper-cli --help          эта справка
  file-pumper-cli --version       версия программы

Накачка файла:
  file-pumper-cli pump <вход> <выход> --size <МБ>
                                   [--method animation|png-chunks|upscale|svg|office|padding]
                                   [--format gif|png|webp]

Спрятать файл в картинке:
  file-pumper-cli embed <носитель> <нагрузка> [выход]
                                   [--copies 1..3] [--compress] [--password ПАРОЛЬ]

Извлечь файл из картинки:
  file-pumper-cli extract <носитель> [выход] [--password ПАРОЛЬ]

Проверить, что лежит внутри:
  file-pumper-cli inspect <носитель>

Те же команды принимает и графическая сборка, но у неё нет консоли:
ошибки она показывает диалогом, поэтому для терминала используйте
file-pumper-cli.
";

struct Flags {
    positional: Vec<String>,
    size: Option<f64>,
    method: Method,
    format: AnimationFormat,
    copies: usize,
    compress: bool,
    password: Option<String>,
}

fn parse_method(value: &str) -> Result<Method, String> {
    match value {
        "animation" => Ok(Method::Animation),
        "png-chunks" => Ok(Method::PngChunks),
        "upscale" => Ok(Method::Upscale),
        "svg" => Ok(Method::Svg),
        "office" => Ok(Method::OfficePdf),
        "padding" => Ok(Method::Padding),
        other => Err(format!("Неизвестный метод: {other}")),
    }
}

fn parse_format(value: &str) -> Result<AnimationFormat, String> {
    match value {
        "gif" => Ok(AnimationFormat::Gif),
        "png" => Ok(AnimationFormat::Png),
        "webp" => Ok(AnimationFormat::Webp),
        other => Err(format!("Неизвестный формат: {other}")),
    }
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = Flags {
            positional: Vec::new(),
            size: None,
            method: Method::Animation,
            format: AnimationFormat::Gif,
            copies: 1,
            compress: false,
            password: None,
        };
        let mut iterator = args.iter();
        let next = |name: &str, iterator: &mut std::slice::Iter<'_, String>| {
            iterator
                .next()
                .cloned()
                .ok_or_else(|| format!("Параметр {name} требует значение."))
        };
        while let Some(argument) = iterator.next() {
            match argument.as_str() {
                "--size" => {
                    let raw = next("--size", &mut iterator)?.replace(',', ".");
                    flags.size = Some(
                        raw.parse::<f64>()
                            .map_err(|_| format!("Неверный размер: {raw}"))?,
                    );
                }
                "--copies" => {
                    let raw = next("--copies", &mut iterator)?;
                    flags.copies = raw
                        .parse::<usize>()
                        .map_err(|_| format!("Неверное число копий: {raw}"))?
                        .clamp(1, 3);
                }
                "--password" => flags.password = Some(next("--password", &mut iterator)?),
                "--compress" => flags.compress = true,
                "--method" => {
                    let raw = next("--method", &mut iterator)?;
                    flags.method = parse_method(&raw)?;
                }
                "--format" => {
                    let raw = next("--format", &mut iterator)?;
                    flags.format = parse_format(&raw)?;
                }
                other if other.starts_with("--") => {
                    return Err(format!("Неизвестный параметр: {other}"));
                }
                other => flags.positional.push(other.to_owned()),
            }
        }
        Ok(flags)
    }

    fn path(&self, index: usize, name: &str) -> Result<PathBuf, String> {
        self.positional
            .get(index)
            .map(PathBuf::from)
            .ok_or_else(|| format!("Укажите путь: {name}."))
    }

    fn options(&self) -> ContainerOptions {
        ContainerOptions {
            compress: self.compress,
            password: self
                .password
                .as_ref()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
        }
    }
}

/// Возвращает код выхода процесса.
pub fn run(args: &[String]) -> i32 {
    match execute(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Ошибка: {error}");
            1
        }
    }
}

/// Выполняет команду и возвращает результат (используется и графической сборкой,
/// у которой нет консоли для печати кода выхода).
pub fn execute(args: &[String]) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        print!("{HELP}");
        return Ok(());
    };
    if matches!(command.as_str(), "--help" | "-h" | "help") {
        print!("{HELP}");
        return Ok(());
    }
    if matches!(command.as_str(), "--version" | "-V" | "version") {
        println!("FilePumper {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let flags = Flags::parse(rest)?;
    match command.as_str() {
        "pump" => command_pump(&flags),
        "embed" => command_embed(&flags),
        "extract" => command_extract(&flags),
        "inspect" => command_inspect(&flags),
        other => Err(format!("Неизвестная команда: {other}. Используйте --help.")),
    }
}

fn command_pump(flags: &Flags) -> Result<(), String> {
    let input = flags.path(0, "вход")?;
    let output = flags.path(1, "выход")?;
    let size_mb = flags
        .size
        .ok_or_else(|| "Укажите размер: --size <МБ>.".to_owned())?;
    if !(size_mb.is_finite() && size_mb > 0.0) {
        return Err("Размер должен быть положительным числом.".into());
    }
    let target_bytes = (size_mb * MIB as f64) as u64;
    let progress = |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<60}");
        let _ = std::io::stderr().flush();
    };
    let quiet = crate::util::CancelToken::new();
    let job = Job::new(&progress, &quiet);
    crate::pumper::run(
        flags.method,
        flags.format,
        &input,
        &output,
        target_bytes,
        &job,
    )?;
    finish_line(&output)
}

fn command_embed(flags: &Flags) -> Result<(), String> {
    let carrier = flags.path(0, "носитель")?;
    let payload_path = flags.path(1, "нагрузка")?;
    let output = match flags.positional.get(2) {
        Some(value) => PathBuf::from(value),
        None => payload::default_output_path(&carrier),
    };
    let progress = |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<60}");
        let _ = std::io::stderr().flush();
    };
    let token = crate::util::CancelToken::new();
    let job = Job::new(&progress, &token);
    let outcome = payload::embed_file(
        &carrier,
        &payload_path,
        &output,
        &flags.options(),
        flags.copies,
        &job,
    )?;
    eprintln!(
        "\rГотово: {} ({}), копий: {}, опции: {}, декодер: {}",
        outcome.output.display(),
        outcome.carrier.label(),
        outcome.copies,
        outcome.options,
        if outcome.verified_with_decoder {
            "файл валиден"
        } else {
            "проверить не удалось"
        }
    );
    Ok(())
}

fn command_extract(flags: &Flags) -> Result<(), String> {
    let carrier = flags.path(0, "носитель")?;
    let password = flags.options().password;
    let output = match flags.positional.get(1) {
        Some(value) => PathBuf::from(value),
        None => {
            let summary = payload::inspect_file(&carrier, password.as_deref())?;
            payload::default_extract_path(&carrier, &summary.info.name)
        }
    };
    let progress = |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<60}");
        let _ = std::io::stderr().flush();
    };
    let token = crate::util::CancelToken::new();
    let job = Job::new(&progress, &token);
    let outcome = payload::extract_to(&carrier, &output, password.as_deref(), &job)?;
    eprintln!(
        "\rГотово: {} ({} байт, целых копий: {})",
        outcome.output.display(),
        outcome.size,
        outcome.copies
    );
    Ok(())
}

fn command_inspect(flags: &Flags) -> Result<(), String> {
    let carrier = flags.path(0, "носитель")?;
    let password = flags.options().password;
    let summary = payload::inspect_file(&carrier, password.as_deref())?;
    let info = &summary.info;
    println!("Носитель: {}", summary.carrier.label());
    println!("Имя нагрузки: {}", info.name);
    println!("Размер: {} байт", info.size);
    println!("Целых копий: {}", info.copies);
    println!(
        "Сжатие: {}",
        if info.compressed {
            "да (deflate)"
        } else {
            "нет"
        }
    );
    println!("Шифрование: {}", if info.encrypted { "да" } else { "нет" });
    if info.legacy {
        println!("Формат: контейнер версии 1");
    }
    println!(
        "Устойчивость к загрузке: {}",
        summary.carrier.survives_upload()
    );
    Ok(())
}

fn finish_line(output: &std::path::Path) -> Result<(), String> {
    let size = std::fs::metadata(output)
        .map_err(|err| err.to_string())?
        .len();
    eprintln!(
        "\rГотово: {} ({:.2} МБ)",
        output.display(),
        size as f64 / MIB as f64
    );
    Ok(())
}
