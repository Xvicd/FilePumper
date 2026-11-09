//! Командная строка: накачка, встраивание, извлечение, проверка и отчёты без GUI.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::batch::{
    self, BatchItem, BatchReport, GenerateTargets, ItemResult, JitterMode, MixMode, NameMode,
    Names, PumpPlan, RandomNames, RandomStyle, SizeJitter, TargetMode,
};
use crate::payload::{self, ContainerOptions};
use crate::pumper::{AnimationFormat, GeneratedKind, Method};
use crate::report;
use crate::scan;
use crate::util::{CancelToken, Job, Reservations, CANCELLED_MESSAGE, MIB};

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
                                   [--vary <МБ> [--vary-mode plus|minus|both]]
                                   [--names size|plain|random]
                                   [--random-style words|chars]
                                   [--random-length N] [--random-alphabet НАБОР]
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
  --vary         разброс размера: вместо ровных 400 МБ файл получит случайный
                 размер в пределах ±указанного объёма (--vary 5 → 395…405 МБ).
                 Разброс считается для каждого файла отдельно, а записанный файл
                 всё равно ровно того размера, который выпал; 0 — выключить
  --vary-mode    куда отклоняться: both — в обе стороны (по умолчанию),
                 plus — только прибавлять, minus — только убавлять
  --names        имена результатов: size — с размером (photo_10MB.png,
                 по умолчанию), plain — без размера (photo.png), random —
                 случайное имя (quiet-river-4821.png или набор символов)
  --random-style вид случайного имени: words — два слова и число
                 (green-apple-4821, по умолчанию), chars — символы из алфавита
  --random-length  сколько символов в случайном имени при --random-style chars
                 (10 по умолчанию, допустимо 3..48)
  --random-alphabet  из каких символов собирать имя (по умолчанию a-z0-9);
                 лишние символы отбрасываются: годятся латиница, цифры, - и _
  --from-dir     добавить все файлы папки, --recursive — вместе с подпапками
  --ext          фильтр расширений для --from-dir (например png,jpg)
  --from-file    текстовый список путей (по одному в строке, # — комментарий)
  --skip-unsupported  молча пропускать файлы, которые метод не поддерживает
  --jobs N       обрабатывать N файлов одновременно (1..16)
  --dry-run      показать план и ничего не записывать
  --verify       после записи проверить размер результата
  --report       сохранить отчёт CSV или JSON (формат по расширению)

Создать файл с нуля (исходник не нужен; внутрь можно спрятать нагрузку):
  file-pumper-cli generate --size <МБ[,МБ...]> [--count N]
                                   [--type bin|png|gif|jpg|webp[,ТИП...]]
                                   [--mix even|random]
                                   [--vary <МБ> [--vary-mode plus|minus|both]]
                                   [--name ОСНОВА] [--names size|plain|random]
                                   [--random-style words|chars]
                                   [--random-length N] [--random-alphabet НАБОР]
                                   [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--payload ФАЙЛ ...] [--copies 1..3]
                                   [--compress] [--password ПАРОЛЬ]
                                   [--dry-run] [--verify]
                                   [--report ОТЧЁТ.csv|ОТЧЁТ.json]

  Содержимое — криптослучайные байты, поэтому архиватор файл не сожмёт:
  zip, gzip и 7z вернут примерно тот же объём.
  --type         bin — обычный файл (по умолчанию); png, gif, jpg, webp —
                 валидная картинка: объём лежит внутри её структуры.
                 Список через запятую создаёт сразу несколько типов
                 (--type png,gif), all — все пять
  --mix          как разложить несколько типов: even — по --count файлов
                 каждого типа (по умолчанию), random — всего --count файлов,
                 тип каждого выбирается случайно из указанных
  --count N      сколько файлов создать: при --mix even на каждый тип, при
                 --mix random всего. --size 5,10 --count 2 --type png,gif даёт
                 восемь файлов (по два на каждый размер и тип)
  --name         основа имени без расширения (по умолчанию file)
  --payload      что спрятать внутрь: контейнер встаёт в заранее посчитанное
                 место, поэтому размер файла остаётся ровно заданным
  Примеры: file-pumper-cli generate --size 10 --type png --name photo
           file-pumper-cli generate --size 5,20 --count 2 --type gif
           file-pumper-cli generate --size 10 --type png,gif,jpg --count 3
           file-pumper-cli generate --size 10 --type bin,png --count 6 --mix random
           file-pumper-cli generate --size 10 --payload secret.pdf --password 1234
           file-pumper-cli generate --size 400 --vary 5 --type png
           размер каждого файла — случайный в пределах 395…405 МБ

Спрятать файлы в картинке (можно несколько носителей и нагрузок):
  file-pumper-cli embed <носитель>... --payload <файл> [--payload <файл>...]
                                   [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--copies 1..3] [--compress] [--password ПАРОЛЬ]
                                   [--pair] [--pair-password ПАРОЛЬ]...
                                   [--names random]
                                   [--dry-run] [--report ОТЧЁТ.csv|ОТЧЁТ.json]
  Несколько --payload упаковываются в один ZIP-бандл внутри контейнера.
  --pair сопоставляет носители и нагрузки по порядку: первая нагрузка идёт в
  первый носитель, вторая во второй и так далее (нагрузок меньше — повторяются,
  больше — делятся поровну и попадают в носитель бандлом).
  --pair-password задаёт пароль пары; флаг можно повторять, последний пароль
  достаётся всем оставшимся парам. Без него работает общий --password.
  --names random даёт результатам случайные имена (quiet-river-4821.png);
  вид задают --random-style, --random-length и --random-alphabet.

Извлечь файлы из картинки (можно несколько носителей):
  file-pumper-cli extract <носитель>... [--out-dir ПАПКА] [--output ФАЙЛ]
                                   [--password ПАРОЛЬ]
  Извлечённый файл сохраняет расширение из имени нагрузки, поэтому система
  знает, чем его открывать. Бандл распаковывается в папку по одному файлу.
  Файлы без нагрузки в пакете не считаются ошибкой: в отчёте видно, что
  можно извлечь и из какого файла. Код выхода 1 — только при реальных ошибках.
  --password можно повторять: пароли перебираются по порядку, поэтому пачка
  файлов с разными паролями извлекается одной командой.

Проверить, что лежит внутри (можно несколько носителей):
  file-pumper-cli inspect <носитель>... [--password ПАРОЛЬ]...
  По каждому файлу печатается отчёт, в конце — итог вида
  «с нагрузкой 2 из 3» и список «что можно извлечь».

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
    vary: Option<f64>,
    vary_mode: JitterMode,
    names: NameMode,
    /// Настройки случайных имён: работают вместе с `--names random`.
    random: RandomNames,
    method: Method,
    format: AnimationFormat,
    copies: usize,
    compress: bool,
    password: Option<String>,
    /// Все пароли из `--password` (флаг можно повторять): для проверки и
    /// извлечения они перебираются по порядку — в пачке бывают разные пароли.
    passwords: Vec<String>,
    payloads: Vec<PathBuf>,
    /// Групповое встраивание: сопоставить носители и нагрузки по порядку.
    pair: bool,
    /// Пароли пар: `--pair-password` можно повторять, лишние пары берут последний.
    pair_passwords: Vec<String>,
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
    generated: Vec<GeneratedKind>,
    mix: MixMode,
    base_name: String,
    count: usize,
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

/// Разброс размера в мегабайтах: запятая тоже работает, отрицательный — ошибка.
fn parse_spread(value: &str) -> Result<f64, String> {
    let raw = value.trim().replace(',', ".").replace(' ', "");
    let spread = raw
        .parse::<f64>()
        .map_err(|_| format!("Неверный разброс: {value}"))?;
    if !spread.is_finite() || spread < 0.0 {
        return Err("Разброс должен быть неотрицательным числом в мегабайтах.".to_owned());
    }
    Ok(spread)
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = Flags {
            positional: Vec::new(),
            sizes: Vec::new(),
            size_mode: TargetMode::Exact,
            vary: None,
            vary_mode: JitterMode::Both,
            names: NameMode::Size,
            random: RandomNames::default(),
            method: Method::Animation,
            format: AnimationFormat::Gif,
            copies: 1,
            compress: false,
            password: None,
            passwords: Vec::new(),
            payloads: Vec::new(),
            pair: false,
            pair_passwords: Vec::new(),
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
            generated: Vec::new(),
            mix: MixMode::default(),
            base_name: "file".to_owned(),
            count: 1,
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
                "--vary" | "--jitter" => {
                    let raw = next("--vary", &mut iterator)?;
                    flags.vary = Some(parse_spread(&raw)?);
                }
                "--vary-mode" | "--jitter-mode" | "--vary-direction" => {
                    let raw = next("--vary-mode", &mut iterator)?;
                    flags.vary_mode = JitterMode::parse(&raw)?;
                }
                "--names" | "--name-mode" => {
                    let raw = next("--names", &mut iterator)?;
                    flags.names = NameMode::parse(&raw)?;
                }
                "--random-style" | "--random-name-style" => {
                    let raw = next("--random-style", &mut iterator)?;
                    flags.random.style = RandomStyle::parse(&raw)?;
                }
                "--random-length" | "--random-len" => {
                    let raw = next("--random-length", &mut iterator)?;
                    let length = raw
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| format!("Неверная длина случайного имени: {raw}"))?;
                    let range = batch::RANDOM_LENGTH_RANGE;
                    if !range.contains(&length) {
                        return Err(format!(
                            "Длина случайного имени — от {} до {} символов.",
                            range.start(),
                            range.end()
                        ));
                    }
                    flags.random.length = length;
                }
                "--random-alphabet" | "--random-chars" => {
                    let raw = next("--random-alphabet", &mut iterator)?;
                    let alphabet = batch::sanitize_alphabet(&raw);
                    if alphabet.is_empty() {
                        return Err(format!(
                            "Алфавит случайного имени пуст: в «{raw}» нет латинских букв, цифр, «-» или «_»."
                        ));
                    }
                    flags.random.alphabet = alphabet;
                }
                "--copies" => {
                    let raw = next("--copies", &mut iterator)?;
                    flags.copies = raw
                        .parse::<usize>()
                        .map_err(|_| format!("Неверное число копий: {raw}"))?
                        .clamp(1, 3);
                }
                "--password" => {
                    let raw = next("--password", &mut iterator)?;
                    if flags.passwords.is_empty() {
                        flags.password = Some(raw.clone());
                    }
                    flags.passwords.push(raw);
                }
                "--payload" => flags
                    .payloads
                    .push(PathBuf::from(next("--payload", &mut iterator)?)),
                "--pair" | "--pairs" | "--pair-by-order" => flags.pair = true,
                "--pair-password" | "--pair-passwords" => {
                    flags
                        .pair_passwords
                        .push(next("--pair-password", &mut iterator)?);
                }
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
                "--type" | "--kind" => {
                    let raw = next("--type", &mut iterator)?;
                    for kind in GeneratedKind::parse_list(&raw)? {
                        if !flags.generated.contains(&kind) {
                            flags.generated.push(kind);
                        }
                    }
                }
                "--mix" => {
                    let raw = next("--mix", &mut iterator)?;
                    flags.mix = MixMode::parse(&raw)?;
                }
                "--name" | "--base-name" => {
                    let raw = next("--name", &mut iterator)?;
                    flags.base_name = raw;
                }
                "--count" => {
                    let raw = next("--count", &mut iterator)?;
                    flags.count = raw
                        .parse::<usize>()
                        .map_err(|_| format!("Неверное число файлов: {raw}"))?
                        .clamp(1, 64);
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

    /// Все пароли для перебора: `--password` можно повторять.
    fn password_candidates(&self) -> Vec<String> {
        self.passwords
            .iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect()
    }

    /// Разброс размера из `--vary` и `--vary-mode`; `None` — разброс выключен.
    fn jitter(&self) -> Result<Option<SizeJitter>, String> {
        SizeJitter::from_mb(self.vary.unwrap_or(0.0), self.vary_mode)
    }

    /// Режим имён вместе с настройками случайных имён.
    fn names_value(&self) -> Names {
        Names::new(self.names).with_random(self.random.clone())
    }

    /// Что создавать: без `--type` — обычный файл, как раньше.
    fn generate_kinds(&self) -> Vec<GeneratedKind> {
        if self.generated.is_empty() {
            vec![GeneratedKind::default()]
        } else {
            self.generated.clone()
        }
    }
}

/// Подпись режима имён для строки плана: у случайного имени ещё и способ.
fn names_label(flags: &Flags) -> String {
    match flags.names {
        NameMode::Random => format!("{} ({})", NameMode::Random.label(), flags.random.label()),
        other => other.label().to_owned(),
    }
}

/// Типы файлов для строки плана: `png, gif`.
fn kinds_label(kinds: &[GeneratedKind]) -> String {
    kinds
        .iter()
        .map(|kind| kind.extension())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Как разложены несколько типов: у одного типа пояснение не нужно.
fn mix_note(kinds: &[GeneratedKind], mix: MixMode) -> String {
    if kinds.len() < 2 {
        return String::new();
    }
    format!(", распределение: «{}»", mix.label())
}

/// Предупреждает, если разброс способен «съесть» весь размер: при `minus` файл
/// может получиться совсем маленьким, а для картинок — меньше служебного минимума.
fn warn_jitter(jitter: Option<SizeJitter>, targets: &[u64]) {
    let Some(jitter) = jitter else {
        return;
    };
    if jitter.mode == JitterMode::Plus {
        return;
    }
    if let Some(smallest) = targets.iter().copied().min() {
        if jitter.spread >= smallest {
            eprintln!(
                "Предупреждение: разброс {} не меньше самого размера ({}) — файл может выйти совсем маленьким.",
                jitter.label(),
                batch::size_label(smallest)
            );
        }
    }
}

/// Приписка к строке плана: `, разброс «±5MB»`.
fn jitter_note(jitter: Option<SizeJitter>) -> String {
    match jitter {
        Some(jitter) => format!(", разброс «{}»", jitter.label()),
        None => String::new(),
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
        "generate" | "make" => command_generate(&flags),
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
    )?
    .with_names(flags.names_value());
    let jitter = flags.jitter()?;
    let plan = plan.with_jitter(jitter);
    warn_jitter(jitter, &plan.targets);
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
            "План: {} файл(ов), метод «{}», режим «{}»{}",
            planned.items.len(),
            flags.method.title(),
            flags.size_mode.label(),
            jitter_note(jitter)
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

/// Что лежит внутри будущего файла — для отчёта и `--dry-run`.
fn payloads_label(payloads: &[PathBuf]) -> String {
    match payloads.len() {
        0 => "нет".to_owned(),
        1 => payloads[0].display().to_string(),
        count => format!("бандл из {count} файлов"),
    }
}

fn command_generate(flags: &Flags) -> Result<(), String> {
    for payload in &flags.payloads {
        if !payload.is_file() {
            return Err(format!("Файл нагрузки не найден: {}", payload.display()));
        }
    }
    let targets = batch::targets_from_mb(&flags.sizes)?;
    let jitter = flags.jitter()?;
    warn_jitter(jitter, &targets);
    let kinds = flags.generate_kinds();
    let spec = GenerateTargets {
        sizes: targets,
        count: flags.count,
        jitter,
        kinds: kinds.clone(),
        mix: flags.mix,
    };
    let mut items = batch::plan_generate(
        &flags.base_name,
        flags.names_value(),
        flags.out_dir.as_deref(),
        &spec,
        &flags.payloads,
    )?;

    if let Some(output) = flags.output.as_ref() {
        if items.len() > 1 {
            return Err(
                "Параметр --output работает только для одного файла: поставьте --count 1 и один --size."
                    .to_owned(),
            );
        }
        for payload in &flags.payloads {
            if crate::util::same_file(payload, output) {
                return Err(
                    "Итоговый файл совпадает с файлом нагрузки. Выберите другое имя.".to_owned(),
                );
            }
        }
        items[0].output = output.clone();
    }

    if flags.dry_run {
        println!(
            "План: {} файл(ов), типы: {}, имена «{}»{}{}",
            items.len(),
            kinds_label(&kinds),
            names_label(flags),
            jitter_note(jitter),
            mix_note(&kinds, flags.mix)
        );
        for item in &items {
            println!(
                "  {} ({}){}",
                item.output.display(),
                batch::size_label(item.target),
                if item.kind.is_image() {
                    " — валидная картинка"
                } else {
                    " — случайные данные"
                }
            );
        }
        println!("  внутрь: {}", payloads_label(&flags.payloads));
        return Ok(());
    }

    let total = items.len();
    let token = CancelToken::new();
    let progress = progress_printer();
    let options = flags.options();
    let mut results: Vec<(PathBuf, ItemResult)> = Vec::with_capacity(total);
    for (index, item) in items.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        let local = |percent: f32, text: &str| {
            let within = percent.clamp(0.0, 100.0) / 100.0;
            let overall = ((index as f32 + within) / total as f32).clamp(0.0, 1.0);
            progress(overall, &format!("{}/{total}: {text}", index + 1));
        };
        let job = Job::new(&local, &token);
        let result = match batch::generate_one(item, &options, flags.copies, &job) {
            Ok(size) => ItemResult::Created {
                output: item.output.clone(),
                size,
            },
            Err(message) => ItemResult::Failed { message },
        };
        results.push((item.output.clone(), result));
    }
    eprintln!();

    if flags.verify {
        for (index, (path, result)) in results.iter_mut().enumerate() {
            let ItemResult::Created { size, .. } = result else {
                continue;
            };
            let expected = items[index].target;
            let actual = std::fs::metadata(&*path)
                .map(|meta| meta.len())
                .unwrap_or(0);
            if actual != expected || *size != expected {
                *result = ItemResult::Failed {
                    message: format!("проверка размера не прошла: {actual} из {expected} байт"),
                };
            }
        }
    }

    let batch_report = BatchReport {
        cancelled: token.is_cancelled(),
        items: results,
    };

    for (path, result) in &batch_report.items {
        match result {
            ItemResult::Created { output, size } => {
                let note = if flags.payloads.is_empty() {
                    String::new()
                } else {
                    format!(", внутрь: {}", payloads_label(&flags.payloads))
                };
                println!(
                    "Готово: {} ({:.2} МБ{note})",
                    output.display(),
                    *size as f64 / MIB as f64
                );
            }
            ItemResult::Failed { message } => {
                eprintln!("Ошибка: {} — {message}", path.display());
            }
            ItemResult::Skipped { reason } => {
                eprintln!("Пропущено: {} — {reason}", path.display());
            }
        }
    }

    if let Some(path) = flags.report_path.as_deref() {
        report::write_report(path, &batch_report)?;
        println!("Отчёт: {}", path.display());
    }

    if batch_report.cancelled {
        return Err(CANCELLED_MESSAGE.to_owned());
    }
    let failed = batch_report.failed();
    if failed > 0 {
        return Err(format!("Не удалось создать {failed} из {total} файлов."));
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
    let passwords: Vec<Option<String>> = flags
        .pair_passwords
        .iter()
        .map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
        .collect();
    // Пары «носитель ← нагрузка». Без --pair каждый носитель получает все
    // нагрузки бандлом, с --pair — по порядку и со своим паролем.
    let specs = if flags.pair {
        payload::group::pair_by_order(&carriers, &payloads, &passwords)?
    } else {
        carriers
            .iter()
            .map(|carrier| payload::group::EmbedPair {
                carrier: carrier.clone(),
                payloads: payloads.clone(),
                password: None,
            })
            .collect()
    };

    let mut plan: Vec<(payload::group::EmbedPair, PathBuf)> = Vec::with_capacity(specs.len());
    for spec in specs {
        let carrier = &spec.carrier;
        let output = match explicit_output.clone() {
            Some(path) => path,
            None => {
                let suggested = payload::default_output_path(carrier);
                let suggested = if flags.names == NameMode::Random {
                    batch::with_random_stem(&suggested, &flags.random)
                } else {
                    suggested
                };
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
        plan.push((spec, output));
    }

    if flags.dry_run {
        println!(
            "План встраивания: носителей {}, нагрузок {}{}",
            plan.len(),
            payloads.len(),
            if payloads.len() > 1 && !flags.pair {
                " (бандл)"
            } else if flags.pair {
                " (по парам)"
            } else {
                ""
            }
        );
        for (spec, output) in &plan {
            let password = match spec.password.as_deref() {
                Some(_) => ", свой пароль",
                None => "",
            };
            println!("  {} -> {}{password}", spec.label(), output.display());
        }
        return Ok(());
    }

    let base_options = flags.options();
    let copies = flags.copies;
    let token = CancelToken::new();
    let progress = progress_printer();
    let job = Job::new(&progress, &token);
    let total = plan.len().max(1) as f32;
    let mut batch_report = BatchReport::default();

    for (index, (spec, output)) in plan.iter().enumerate() {
        let carrier = &spec.carrier;
        let local = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            progress(overall, text);
        };
        let item_job = job.child(&local);
        let options = spec.options(&base_options);
        let result = match spec.payloads.first() {
            Some(first) if !spec.bundle() => {
                payload::embed_file(carrier, first, output, &options, copies, &item_job)
            }
            Some(_) => {
                payload::embed_bundle(carrier, &spec.payloads, output, &options, copies, &item_job)
            }
            None => Err("Не указан ни один файл нагрузки.".to_owned()),
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

    let passwords = flags.password_candidates();
    let progress = progress_printer();
    let token = CancelToken::new();
    let job = Job::new(&progress, &token);

    // Точное имя результата задаётся только для одного носителя и одного файла.
    if let Some(output) = explicit_output {
        let carrier = &carriers[0];
        let (summary, password) = payload::group::probe(carrier, &passwords)?;
        if summary.info.bundle {
            return Err(format!(
                "{} — внутри бандл, он распаковывается в папку (--out-dir), а не в один файл.",
                carrier.display()
            ));
        }
        return match payload::extract_to(carrier, &output, password.as_deref(), &job) {
            Ok(outcome) => {
                eprintln!();
                println!(
                    "Готово: {} -> {} ({} байт, целых копий: {})",
                    carrier.display(),
                    outcome.output.display(),
                    outcome.size,
                    outcome.copies
                );
                Ok(())
            }
            Err(error) => Err(format!("{} — {error}", carrier.display())),
        };
    }

    command_extract_many(&carriers, flags.out_dir.as_deref(), &passwords, &job)
}

/// Извлечение из нескольких носителей сразу: обычные файлы не считаются ошибкой,
/// в отчёте видно, что можно извлечь и из какого файла.
fn command_extract_many(
    carriers: &[PathBuf],
    out_dir: Option<&Path>,
    passwords: &[String],
    job: &Job<'_>,
) -> Result<(), String> {
    let batch = payload::group::extract_many(carriers, out_dir, passwords, job)?;
    eprintln!();

    let mut failed: Vec<String> = Vec::new();
    for item in &batch.items {
        match &item.result {
            payload::group::ExtractResult::File(outcome) => println!(
                "Готово: {} -> {} ({} байт, целых копий: {})",
                item.carrier.display(),
                outcome.output.display(),
                outcome.size,
                outcome.copies
            ),
            payload::group::ExtractResult::Bundle(files) => {
                let folder = files
                    .first()
                    .and_then(|(path, _)| path.parent())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "папка".to_owned());
                println!(
                    "Готово: {} -> {} (файлов: {})",
                    item.carrier.display(),
                    folder,
                    files.len()
                );
                for (path, size) in files {
                    println!("  {} ({} байт)", path.display(), size);
                }
            }
            payload::group::ExtractResult::Empty => {
                if carriers.len() > 1 {
                    println!("Нагрузки нет: {} — обычный файл", item.carrier.display());
                } else {
                    failed.push(format!(
                        "{} — {}",
                        item.carrier.display(),
                        payload::PAYLOAD_NOT_FOUND
                    ));
                }
            }
            payload::group::ExtractResult::Error(error) => {
                failed.push(format!("{} — {error}", item.carrier.display()));
            }
        }
    }

    if carriers.len() > 1 {
        println!();
        println!("Итог: {}", batch.summary());
        println!("Что где нашлось:");
        for item in &batch.items {
            println!("  {}", item.describe());
        }
    }

    if failed.is_empty() {
        return Ok(());
    }
    for line in &failed {
        eprintln!("Ошибка: {line}");
    }
    if carriers.len() == 1 {
        return Err(failed.join("\n"));
    }
    Err(format!(
        "Не удалось обработать {} из {} носителей.",
        failed.len(),
        carriers.len()
    ))
}

fn command_inspect(flags: &Flags) -> Result<(), String> {
    let carriers = flags.paths();
    if carriers.is_empty() {
        return Err("Укажите путь: носитель.".to_owned());
    }
    let items = payload::group::scan_many(&carriers, &flags.password_candidates(), &Job::quiet());
    let multiple = carriers.len() > 1;
    for item in &items {
        if multiple {
            println!("=== {} ===", item.path.display());
        }
        match &item.result {
            payload::group::ScanResult::Found(summary) => {
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
                    let mut entries = payload::payload_entries(&item.path, None);
                    for candidate in flags.password_candidates() {
                        if entries.is_ok() {
                            break;
                        }
                        entries = payload::payload_entries(&item.path, Some(candidate.as_str()));
                    }
                    match entries {
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
            payload::group::ScanResult::Empty => {
                println!("Нагрузки нет: {} — обычный файл", item.path.display());
            }
            payload::group::ScanResult::Error(error) => {
                eprintln!("Ошибка: {} — {error}", item.path.display());
            }
        }
    }

    if multiple {
        println!();
        println!("Итог: {}", payload::group::scan_summary(&items));
        println!("Что можно извлечь:");
        for item in &items {
            println!("  {}", item.describe());
        }
    }

    let failed = items
        .iter()
        .filter(|item| matches!(item.result, payload::group::ScanResult::Error(_)))
        .count();
    if failed > 0 {
        return Err(format!(
            "Не удалось проверить {failed} из {} файлов.",
            carriers.len()
        ));
    }
    if !multiple && !items.iter().any(|item| item.is_found()) {
        return Err(payload::PAYLOAD_NOT_FOUND.to_owned());
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
