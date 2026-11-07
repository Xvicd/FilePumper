//! Командная строка: накачка, встраивание, извлечение, проверка и отчёты без GUI.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::batch::{self, BatchItem, BatchReport, ItemResult, PumpPlan, TargetMode};
use crate::payload::{self, ContainerOptions};
use crate::pumper::{AnimationFormat, Method};
use crate::report;
use crate::scan;
use crate::util::{
    sanitize_file_name, unique_path, CancelToken, Job, Reservations, CANCELLED_MESSAGE, MIB,
};

const HELP: &str = "\
FilePumper

Использование:
  file-pumper                     запустить графический интерфейс (без консоли)
  file-pumper-cli                 эта справка
  file-pumper-cli --help          эта справка
  file-pumper-cli --version       версия программы

Накачка файлов (можно сразу несколько):
  file-pumper-cli pump <вход>... --size <МБ[,МБ...]>
                                   [--size-mode exact|at-least|add]
                                   [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--method animation|png-chunks|upscale|svg|office|padding]
                                   [--format gif|png|webp]
                                   [--from-dir ПАПКА [--recursive] [--ext png,jpg]]
                                   [--from-file СПИСОК] [--skip-unsupported]
                                   [--jobs N] [--dry-run] [--verify]
                                   [--report ОТЧЁТ.csv|ОТЧЁТ.json]

  --size         размер; список через запятую задаёт размер каждому файлу
                 (дробную часть пишите через точку: 1.5)
  --size-mode    exact — ровно столько (по умолчанию), at-least — не меньше,
                 add — прибавить указанный объём к каждому файлу
  --from-dir     добавить все файлы папки, --recursive — вместе с подпапками
  --ext          фильтр расширений для --from-dir (например png,jpg)
  --from-file    текстовый список путей (по одному в строке, # — комментарий)
  --skip-unsupported  молча пропускать файлы, которые метод не поддерживает
  --jobs N       обрабатывать N файлов одновременно (1..16)
  --dry-run      показать план и ничего не записывать
  --verify       после записи проверить размер результата
  --report       сохранить отчёт CSV или JSON (формат по расширению)

Спрятать файлы в картинке (можно несколько носителей и нагрузок):
  file-pumper-cli embed <носитель>... --payload <файл> [--payload <файл>...]
                                   [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--copies 1..3] [--compress] [--password ПАРОЛЬ]
                                   [--dry-run] [--report ОТЧЁТ.csv|ОТЧЁТ.json]
  Несколько --payload упаковываются в один ZIP-бандл внутри контейнера.

Извлечь файлы из картинки (можно несколько носителей):
  file-pumper-cli extract <носитель>... [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--password ПАРОЛЬ]
  Бандл распаковывается в папку по одному файлу.

Проверить, что лежит внутри (можно несколько носителей):
  file-pumper-cli inspect <носитель>... [--password ПАРОЛЬ]

Проверить целостность и контрольные суммы:
  file-pumper-cli verify <носитель>... [--password ПАРОЛЬ]

Пакетная обработка: файлы обрабатываются по очереди (или параллельно с --jobs),
ошибка одного файла не прерывает остальные. Код выхода 1, если хоть один файл
не удался.

Те же команды принимает и графическая сборка, но у неё нет консоли:
ошибки она показывает диалогом, поэтому для терминала используйте
file-pumper-cli.
";

struct Flags {
    positional: Vec<String>,
    sizes: Vec<f64>,
    size_mode: TargetMode,
    method: Method,
    format: AnimationFormat,
    copies: usize,
    compress: bool,
    password: Option<String>,
    payloads: Vec<PathBuf>,
    out_dir: Option<PathBuf>,
    output: Option<PathBuf>,
    from_dir: Option<PathBuf>,
    recursive: bool,
    extensions: Vec<String>,
    from_file: Option<PathBuf>,
    skip_unsupported: bool,
    dry_run: bool,
    jobs: usize,
    report_path: Option<PathBuf>,
    verify: bool,
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

fn parse_sizes(value: &str) -> Result<Vec<f64>, String> {
    let mut sizes = Vec::new();
    for item in value.split([',', ';']) {
        let raw = item.trim().replace(' ', "");
        if raw.is_empty() {
            continue;
        }
        sizes.push(
            raw.parse::<f64>()
                .map_err(|_| format!("Неверный размер: {raw}"))?,
        );
    }
    if sizes.is_empty() {
        return Err("Неверный размер: укажите число в мегабайтах.".to_owned());
    }
    Ok(sizes)
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = Flags {
            positional: Vec::new(),
            sizes: Vec::new(),
            size_mode: TargetMode::Exact,
            method: Method::Animation,
            format: AnimationFormat::Gif,
            copies: 1,
            compress: false,
            password: None,
            payloads: Vec::new(),
            out_dir: None,
            output: None,
            from_dir: None,
            recursive: false,
            extensions: Vec::new(),
            from_file: None,
            skip_unsupported: false,
            dry_run: false,
            jobs: 1,
            report_path: None,
            verify: false,
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
                    let raw = next("--size", &mut iterator)?;
                    flags.sizes = parse_sizes(&raw)?;
                }
                "--size-mode" => {
                    let raw = next("--size-mode", &mut iterator)?;
                    flags.size_mode = TargetMode::parse(&raw)?;
                }
                "--copies" => {
                    let raw = next("--copies", &mut iterator)?;
                    flags.copies = raw
                        .parse::<usize>()
                        .map_err(|_| format!("Неверное число копий: {raw}"))?
                        .clamp(1, 3);
                }
                "--password" => flags.password = Some(next("--password", &mut iterator)?),
                "--payload" => flags
                    .payloads
                    .push(PathBuf::from(next("--payload", &mut iterator)?)),
                "--out-dir" => {
                    flags.out_dir = Some(PathBuf::from(next("--out-dir", &mut iterator)?));
                }
                "--output" => {
                    flags.output = Some(PathBuf::from(next("--output", &mut iterator)?));
                }
                "--from-dir" => {
                    flags.from_dir = Some(PathBuf::from(next("--from-dir", &mut iterator)?));
                }
                "--from-file" => {
                    flags.from_file = Some(PathBuf::from(next("--from-file", &mut iterator)?));
                }
                "--ext" => {
                    let raw = next("--ext", &mut iterator)?;
                    flags.extensions = scan::parse_extensions(&raw);
                }
                "--jobs" => {
                    let raw = next("--jobs", &mut iterator)?;
                    flags.jobs = raw
                        .parse::<usize>()
                        .map_err(|_| format!("Неверное число потоков: {raw}"))?
                        .clamp(1, 16);
                }
                "--report" => {
                    flags.report_path = Some(PathBuf::from(next("--report", &mut iterator)?));
                }
                "--recursive" => flags.recursive = true,
                "--skip-unsupported" => flags.skip_unsupported = true,
                "--dry-run" => flags.dry_run = true,
                "--verify" => flags.verify = true,
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

    /// Все позиционные аргументы — список входных файлов.
    fn paths(&self) -> Vec<PathBuf> {
        self.positional.iter().map(PathBuf::from).collect()
    }

    /// Позиционные входы + `--from-file` + `--from-dir`, без повторов.
    fn inputs(&self) -> Result<Vec<PathBuf>, String> {
        let mut paths = self.paths();
        if let Some(list) = self.from_file.as_deref() {
            paths.extend(scan::read_path_list(list)?);
        }
        if let Some(directory) = self.from_dir.as_deref() {
            paths.extend(scan::collect_from_dir(
                directory,
                self.recursive,
                &self.extensions,
            )?);
        }
        Ok(scan::dedupe(paths))
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
            bundle: false,
        }
    }

    fn password(&self) -> Option<String> {
        self.options().password
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
        "verify" => command_verify(&flags),
        other => Err(format!("Неизвестная команда: {other}. Используйте --help.")),
    }
}

fn progress_printer() -> impl Fn(f32, &str) {
    |percent: f32, text: &str| {
        eprint!("\r{percent:5.1}% {text:<70}");
        let _ = std::io::stderr().flush();
    }
}

fn command_pump(flags: &Flags) -> Result<(), String> {
    let mut inputs = flags.inputs()?;
    if inputs.is_empty() {
        return Err("Укажите путь: вход (или --from-dir / --from-file).".to_owned());
    }
    let explicit_output = flags
        .output
        .clone()
        .or_else(|| flags.legacy_output(&mut inputs));
    if explicit_output.is_some() && inputs.len() > 1 {
        return Err("Параметр --output работает только с одним входным файлом.".to_owned());
    }
    let plan = PumpPlan::new(
        flags.method,
        flags.format,
        flags.out_dir.clone(),
        flags.size_mode,
        &flags.sizes,
    )?;
    plan.ensure_sizes_match(inputs.len())?;

    let mut planned = match explicit_output {
        Some(output) => {
            let input = &inputs[0];
            if !input.is_file() {
                return Err(format!("Файл не найден: {}", input.display()));
            }
            if crate::util::same_file(input, &output) {
                return Err(
                    "Результат совпадает с исходным файлом: выберите другое имя.".to_owned(),
                );
            }
            let input_len = std::fs::metadata(input)
                .map_err(|err| err.to_string())?
                .len();
            batch::PlannedBatch {
                items: vec![BatchItem::with_output(
                    input.clone(),
                    output,
                    plan.target_for(0, input_len),
                )],
                rejected: Vec::new(),
            }
        }
        None => plan.build(&inputs),
    };

    // Отсев файлов, которые метод заведомо не осилит: без флага они попадут
    // в отчёт как ошибки уже во время работы.
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    if flags.skip_unsupported {
        planned.items.retain(
            |item| match batch::support_error(flags.method, &item.input) {
                Some(reason) => {
                    skipped.push((item.input.clone(), reason));
                    false
                }
                None => true,
            },
        );
    }

    if let Some(target) = planned.items.iter().map(|item| item.target).max() {
        for warning in scan::limits_warnings(target, flags.method) {
            eprintln!("Предупреждение: {warning}");
        }
    }

    if flags.dry_run {
        println!(
            "План: {} файл(ов), метод «{}», режим «{}»",
            planned.items.len(),
            flags.method.title(),
            flags.size_mode.label()
        );
        for item in &planned.items {
            let note = match batch::support_error(flags.method, &item.input) {
                Some(reason) => format!(" — внимание: {reason}"),
                None => String::new(),
            };
            println!(
                "  {} -> {} ({}){note}",
                item.input.display(),
                item.output.display(),
                batch::size_label(item.target)
            );
        }
        for (input, reason) in planned.rejected.iter().chain(skipped.iter()) {
            println!("  пропуск {} — {reason}", input.display());
        }
        return Ok(());
    }

    if planned.items.is_empty() {
        return Err("Не удалось подготовить ни один файл к обработке.".to_owned());
    }

    let token = CancelToken::new();
    let progress = progress_printer();
    let mut batch_report = if flags.jobs > 1 {
        batch::pump_parallel(
            &planned.items,
            flags.method,
            flags.format,
            flags.jobs,
            &progress,
            &token,
        )
    } else {
        let job = Job::new(&progress, &token);
        batch::pump(&planned.items, flags.method, flags.format, &job)
    };
    eprintln!();

    if flags.verify {
        for (input, result) in batch_report.items.iter_mut() {
            let ItemResult::Created { size, .. } = result else {
                continue;
            };
            let Some(item) = planned.items.iter().find(|item| &item.input == input) else {
                continue;
            };
            if *size < item.target {
                *result = ItemResult::Failed {
                    message: format!(
                        "проверка размера не прошла: {} из {} байт",
                        size, item.target
                    ),
                };
            }
        }
    }

    let mut items = Vec::with_capacity(skipped.len() + batch_report.items.len());
    for (input, reason) in skipped {
        items.push((input, ItemResult::Skipped { reason }));
    }
    items.extend(std::mem::take(&mut batch_report.items));
    batch_report.items = items;

    for (input, result) in &batch_report.items {
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
            ItemResult::Skipped { reason } => {
                eprintln!("Пропущено: {} — {reason}", input.display());
            }
        }
    }
    for (input, reason) in &planned.rejected {
        eprintln!("Пропущено: {} — {reason}", input.display());
    }

    if let Some(path) = flags.report_path.as_deref() {
        report::write_report(path, &batch_report)?;
        println!("Отчёт: {}", path.display());
    }

    if batch_report.cancelled {
        return Err(CANCELLED_MESSAGE.to_owned());
    }
    let failed = batch_report.failed() + planned.rejected.len();
    if failed > 0 {
        return Err(format!(
            "Не удалось обработать {failed} из {} файлов.",
            inputs.len()
        ));
    }
    Ok(())
}

fn command_embed(flags: &Flags) -> Result<(), String> {
    let mut carriers = flags.paths();
    let mut payloads = flags.payloads.clone();
    let mut explicit_output = flags.output.clone();

    if payloads.is_empty() {
        // Устаревший синтаксис: embed <носитель> <нагрузка> [выход].
        if flags.positional.len() < 2 {
            return Err("Укажите файл нагрузки: --payload ФАЙЛ.".to_owned());
        }
        if flags.positional.len() > 3 {
            return Err(
                "Слишком много путей: укажите один носитель, одну нагрузку и, при желании, выход."
                    .to_owned(),
            );
        }
        carriers = vec![PathBuf::from(&flags.positional[0])];
        payloads = vec![PathBuf::from(&flags.positional[1])];
        if let Some(third) = flags.positional.get(2) {
            explicit_output = Some(PathBuf::from(third));
        }
    }

    if carriers.is_empty() {
        return Err("Укажите путь: носитель.".to_owned());
    }
    if explicit_output.is_some() && carriers.len() > 1 {
        return Err("Параметр --output работает только с одним носителем.".to_owned());
    }
    for carrier in &carriers {
        if !carrier.is_file() {
            return Err(format!("Файл не найден: {}", carrier.display()));
        }
    }
    for payload in &payloads {
        if !payload.is_file() {
            return Err(format!("Файл нагрузки не найден: {}", payload.display()));
        }
    }
    if let Some(directory) = flags.out_dir.as_deref() {
        if !flags.dry_run && !directory.is_dir() {
            std::fs::create_dir_all(directory).map_err(|err| {
                format!("Не удалось создать папку {}: {err}", directory.display())
            })?;
        }
    }

    let mut reservations = Reservations::new();
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(carriers.len());
    for carrier in &carriers {
        let output = match explicit_output.clone() {
            Some(path) => path,
            None => {
                let suggested = payload::default_output_path(carrier);
                let candidate = match flags.out_dir.as_deref() {
                    Some(directory) => directory.join(
                        suggested
                            .file_name()
                            .map(Path::new)
                            .unwrap_or_else(|| Path::new("carrier_with_payload")),
                    ),
                    None => suggested,
                };
                reservations.reserve(&candidate)
            }
        };
        if crate::util::same_file(carrier, &output) {
            return Err(format!(
                "Итоговый файл совпадает с носителем {}: выберите другое имя.",
                carrier.display()
            ));
        }
        plan.push((carrier.clone(), output));
    }

    if flags.dry_run {
        println!(
            "План встраивания: носителей {}, нагрузок {}{}",
            plan.len(),
            payloads.len(),
            if payloads.len() > 1 {
                " (бандл)"
            } else {
                ""
            }
        );
        for (carrier, output) in &plan {
            println!("  {} -> {}", carrier.display(), output.display());
        }
        return Ok(());
    }

    let options = flags.options();
    let copies = flags.copies;
    let token = CancelToken::new();
    let progress = progress_printer();
    let job = Job::new(&progress, &token);
    let total = plan.len().max(1) as f32;
    let mut batch_report = BatchReport::default();

    for (index, (carrier, output)) in plan.iter().enumerate() {
        let local = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            progress(overall, text);
        };
        let item_job = job.child(&local);
        let result = if payloads.len() > 1 {
            payload::embed_bundle(carrier, &payloads, output, &options, copies, &item_job)
        } else {
            payload::embed_file(carrier, &payloads[0], output, &options, copies, &item_job)
        };
        eprintln!();
        match result {
            Ok(outcome) => {
                println!(
                    "Готово: {} -> {} ({}), копий: {}, опции: {}, декодер: {}",
                    carrier.display(),
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
                batch_report.items.push((
                    carrier.clone(),
                    ItemResult::Created {
                        output: outcome.output,
                        size: outcome.output_len,
                    },
                ));
            }
            Err(error) => {
                eprintln!("Ошибка: {} — {error}", carrier.display());
                batch_report
                    .items
                    .push((carrier.clone(), ItemResult::Failed { message: error }));
            }
        }
    }

    if let Some(path) = flags.report_path.as_deref() {
        report::write_report(path, &batch_report)?;
        println!("Отчёт: {}", path.display());
    }
    let failed = batch_report.failed();
    if failed > 0 {
        return Err(format!(
            "Не удалось обработать {failed} из {} носителей.",
            plan.len()
        ));
    }
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

    let password = flags.password();
    let progress = progress_printer();
    let token = CancelToken::new();
    let job = Job::new(&progress, &token);
    let total = carriers.len().max(1) as f32;
    let mut failed = 0_usize;

    for (index, carrier) in carriers.iter().enumerate() {
        let local = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            progress(overall, text);
        };
        let item_job = job.child(&local);

        let summary = match payload::inspect_file(carrier, password.as_deref()) {
            Ok(summary) => summary,
            Err(error) => {
                failed += 1;
                eprintln!("Ошибка: {} — {error}", carrier.display());
                continue;
            }
        };

        if summary.info.bundle {
            if explicit_output.is_some() {
                failed += 1;
                eprintln!(
                    "Ошибка: {} — внутри бандл, он распаковывается в папку (--out-dir), а не в один файл.",
                    carrier.display()
                );
                continue;
            }
            let target_dir = flags.out_dir.clone().unwrap_or_else(|| {
                carrier
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            match payload::extract_bundle_to(carrier, &target_dir, password.as_deref(), &item_job) {
                Ok(files) => {
                    eprintln!();
                    println!(
                        "Готово: {} -> {} (файлов: {})",
                        carrier.display(),
                        target_dir.display(),
                        files.len()
                    );
                    for (path, size) in &files {
                        println!("  {} ({} байт)", path.display(), size);
                    }
                }
                Err(error) => {
                    failed += 1;
                    eprintln!("Ошибка: {} — {error}", carrier.display());
                }
            }
            continue;
        }

        let output = match explicit_output.clone() {
            Some(path) => path,
            None => {
                let candidate = match flags.out_dir.as_deref() {
                    Some(directory) => directory.join(sanitize_file_name(&summary.info.name)),
                    None => payload::default_extract_path(carrier, &summary.info.name),
                };
                unique_path(&candidate)
            }
        };
        match payload::extract_to(carrier, &output, password.as_deref(), &item_job) {
            Ok(outcome) => {
                eprintln!();
                println!(
                    "Готово: {} -> {} ({} байт, целых копий: {})",
                    carrier.display(),
                    outcome.output.display(),
                    outcome.size,
                    outcome.copies
                );
            }
            Err(error) => {
                failed += 1;
                eprintln!("Ошибка: {} — {error}", carrier.display());
            }
        }
    }

    eprintln!();
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
    let password = flags.password();
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
        if info.bundle {
            println!("Содержимое: бандл, несколько файлов (ZIP внутри контейнера)");
            match payload::payload_entries(carrier, password.as_deref()) {
                Ok(entries) => {
                    for entry in &entries {
                        println!("  {} — {} байт", entry.name, entry.size);
                    }
                }
                Err(error) => println!("  оглавление недоступно: {error}"),
            }
        }
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

fn command_verify(flags: &Flags) -> Result<(), String> {
    let carriers = flags.paths();
    if carriers.is_empty() {
        return Err("Укажите путь: носитель.".to_owned());
    }
    let password = flags.password();
    let multiple = carriers.len() > 1;
    let mut failed = 0_usize;
    for carrier in &carriers {
        if multiple {
            println!("=== {} ===", carrier.display());
        }
        match payload::verify_file(carrier, password.as_deref()) {
            Ok(outcome) => {
                println!("Файл: {}", carrier.display());
                println!("Нагрузка: {} ({} байт)", outcome.name, outcome.size);
                println!("Целых копий: {}", outcome.copies);
                println!("SHA-256: {}", outcome.sha256);
                if outcome.bundle {
                    println!("Бандл: {} файл(ов)", outcome.entries.len());
                    for entry in &outcome.entries {
                        println!("  {} — {} байт", entry.name, entry.size);
                    }
                }
                println!("Проверка: OK");
            }
            Err(error) => {
                failed += 1;
                eprintln!("Ошибка: {} — {error}", carrier.display());
            }
        }
    }
    if failed > 0 {
        return Err(format!(
            "Проверка не прошла для {failed} из {} файлов.",
            carriers.len()
        ));
    }
    Ok(())
}
