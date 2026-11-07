//! Командная строка: накачка, встраивание, извлечение и проверка без GUI.

use std::io::Write;
use std::path::PathBuf;

use crate::batch::{self, BatchItem, ItemResult};
use crate::payload::{self, ContainerOptions};
use crate::pumper::{AnimationFormat, Method};
use crate::util::{sanitize_file_name, unique_path, CancelToken, Job, CANCELLED_MESSAGE, MIB};

const HELP: &str = "\
FilePumper

Использование:
  file-pumper                     запустить графический интерфейс (без консоли)
  file-pumper-cli                 эта справка
  file-pumper-cli --help          эта справка
  file-pumper-cli --version       версия программы

Накачка файла (можно сразу несколько файлов):
  file-pumper-cli pump <вход>... --size <МБ>
                                   [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--method animation|png-chunks|upscale|svg|office|padding]
                                   [--format gif|png|webp]
                                   --out-dir  все результаты в одну папку
                                   --output   точное имя результата (один вход)

Спрятать файл в картинке:
  file-pumper-cli embed <носитель> <нагрузка> [выход]
                                   [--copies 1..3] [--compress] [--password ПАРОЛЬ]

Извлечь файл из картинки (можно несколько носителей):
  file-pumper-cli extract <носитель>... [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--password ПАРОЛЬ]

Проверить, что лежит внутри (можно несколько носителей):
  file-pumper-cli inspect <носитель>... [--password ПАРОЛЬ]

Пакетная обработка: файлы обрабатываются по очереди, ошибка одного файла не
прерывает остальные. Код выхода 1, если хоть один файл не удался.

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
    out_dir: Option<PathBuf>,
    output: Option<PathBuf>,
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
            out_dir: None,
            output: None,
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
                "--out-dir" => {
                    flags.out_dir = Some(PathBuf::from(next("--out-dir", &mut iterator)?));
                }
                "--output" => {
                    flags.output = Some(PathBuf::from(next("--output", &mut iterator)?));
                }
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

    /// Все позиционные аргументы — список входных файлов.
    fn paths(&self) -> Vec<PathBuf> {
        self.positional.iter().map(PathBuf::from).collect()
    }

    /// Совместимость со старым синтаксисом `pump <вход> <выход>` и
    /// `extract <носитель> <выход>`: если второй путь ещё не существует, он
    /// считается выходным файлом. Явные `--output` и `--out-dir` отменяют правило.
    fn legacy_output(&self, inputs: &mut Vec<PathBuf>) -> Option<PathBuf> {
        if self.output.is_some() || self.out_dir.is_some() || inputs.len() != 2 {
            return None;
        }
        if inputs[1].exists() {
            return None;
        }
        eprintln!(
            "Предупреждение: второй путь понят как выходной файл (устаревший синтаксис).\n\
             Для одного файла используйте --output, для пакета — --out-dir."
        );
        inputs.pop()
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
    let size_mb = flags
        .size
        .ok_or_else(|| "Укажите размер: --size <МБ>.".to_owned())?;
    if !(size_mb.is_finite() && size_mb > 0.0) {
        return Err("Размер должен быть положительным числом.".into());
    }
    let target_bytes = (size_mb * MIB as f64) as u64;
    if target_bytes == 0 {
        return Err("Размер слишком мал: укажите хотя бы 0,001 МБ.".into());
    }

    let mut inputs = flags.paths();
    if inputs.is_empty() {
        return Err("Укажите путь: вход.".to_owned());
    }
    let explicit_output = flags
        .output
        .clone()
        .or_else(|| flags.legacy_output(&mut inputs));

    let mut items: Vec<BatchItem> = Vec::new();
    let mut rejected: Vec<(PathBuf, String)> = Vec::new();
    match explicit_output {
        Some(output) => {
            if inputs.len() > 1 {
                return Err("Параметр --output работает только с одним входным файлом.".to_owned());
            }
            let input = inputs.remove(0);
            if !input.is_file() {
                return Err(format!("Файл не найден: {}", input.display()));
            }
            if crate::util::same_file(&input, &output) {
                return Err(
                    "Результат совпадает с исходным файлом: выберите другое имя.".to_owned(),
                );
            }
            items.push(BatchItem { input, output });
        }
        None => {
            let out_dir = flags.out_dir.as_deref();
            if let Some(directory) = out_dir {
                if !directory.is_dir() {
                    std::fs::create_dir_all(directory).map_err(|err| {
                        format!("Не удалось создать папку {}: {err}", directory.display())
                    })?;
                }
            }
            for input in &inputs {
                match BatchItem::new(input, target_bytes, flags.method, flags.format, out_dir) {
                    Ok(item) => items.push(item),
                    Err(message) => rejected.push((input.clone(), message)),
                }
            }
        }
    }
    for (input, message) in &rejected {
        eprintln!("Пропущено: {} — {message}", input.display());
    }
    if items.is_empty() {
        return Err("Не удалось подготовить ни один файл к обработке.".to_owned());
    }

    let token = CancelToken::new();
    let progress = |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<70}");
        let _ = std::io::stderr().flush();
    };
    let job = Job::new(&progress, &token);
    let report = batch::pump(&items, flags.method, flags.format, target_bytes, &job);
    eprintln!();

    for (input, result) in &report.items {
        match result {
            ItemResult::Created { output, size } => println!(
                "Готово: {} -> {} ({:.2} МБ)",
                input.display(),
                output.display(),
                *size as f64 / MIB as f64
            ),
            ItemResult::Failed { message } => {
                eprintln!("Ошибка: {} — {message}", input.display());
            }
            ItemResult::Skipped => eprintln!("Пропущено: {}", input.display()),
        }
    }

    if report.cancelled {
        return Err(CANCELLED_MESSAGE.to_owned());
    }
    let failed = report.failed() + rejected.len();
    if failed > 0 {
        return Err(format!(
            "Не удалось обработать {failed} из {} файлов.",
            inputs.len()
        ));
    }
    Ok(())
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
    let mut carriers = flags.paths();
    if carriers.is_empty() {
        return Err("Укажите путь: носитель.".to_owned());
    }
    let explicit_output = flags
        .output
        .clone()
        .or_else(|| flags.legacy_output(&mut carriers));
    if explicit_output.is_some() && carriers.len() > 1 {
        return Err("Параметр --output работает только с одним носителем.".to_owned());
    }
    if let Some(directory) = flags.out_dir.as_deref() {
        if !directory.is_dir() {
            std::fs::create_dir_all(directory).map_err(|err| {
                format!("Не удалось создать папку {}: {err}", directory.display())
            })?;
        }
    }
    let password = flags.options().password;
    let progress = |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<60}");
        let _ = std::io::stderr().flush();
    };
    let token = CancelToken::new();
    let job = Job::new(&progress, &token);
    let mut failed = 0_usize;

    for carrier in &carriers {
        let output = match explicit_output.clone() {
            Some(path) => path,
            None => match payload::inspect_file(carrier, password.as_deref()) {
                Ok(summary) => {
                    let candidate = match flags.out_dir.as_deref() {
                        Some(directory) => directory.join(sanitize_file_name(&summary.info.name)),
                        None => payload::default_extract_path(carrier, &summary.info.name),
                    };
                    unique_path(&candidate)
                }
                Err(error) => {
                    failed += 1;
                    eprintln!("Ошибка: {} — {error}", carrier.display());
                    continue;
                }
            },
        };
        match payload::extract_to(carrier, &output, password.as_deref(), &job) {
            Ok(outcome) => println!(
                "Готово: {} -> {} ({} байт, целых копий: {})",
                carrier.display(),
                outcome.output.display(),
                outcome.size,
                outcome.copies
            ),
            Err(error) => {
                failed += 1;
                eprintln!("Ошибка: {} — {error}", carrier.display());
            }
        }
    }

    if failed > 0 {
        return Err(format!(
            "Не удалось обработать {failed} из {} носителей.",
            carriers.len()
        ));
    }
    Ok(())
}

fn command_inspect(flags: &Flags) -> Result<(), String> {
    let carriers = flags.paths();
    if carriers.is_empty() {
        return Err("Укажите путь: носитель.".to_owned());
    }
    let password = flags.options().password;
    let multiple = carriers.len() > 1;
    let mut failed = 0_usize;
    for carrier in &carriers {
        if multiple {
            println!("=== {} ===", carrier.display());
        }
        let summary = match payload::inspect_file(carrier, password.as_deref()) {
            Ok(summary) => summary,
            Err(error) => {
                failed += 1;
                eprintln!("Ошибка: {} — {error}", carrier.display());
                continue;
            }
        };
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
    }
    if failed > 0 {
        return Err(format!(
            "Не удалось проверить {failed} из {} файлов.",
            carriers.len()
        ));
    }
    Ok(())
}
