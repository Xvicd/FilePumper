//! Пакетная накачка: несколько файлов за один запуск.
//!
//! Сами методы накачки работают с одной парой «вход → выход», поэтому пакетность
//! живёт здесь: план (размеры, имена результатов), последовательный или
//! параллельный прогон, общий прогресс и отчёт, в котором сбой одного файла не
//! отменяет остальные.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::payload::{self, ContainerOptions};
use crate::pumper::{self, generate, AnimationFormat, GeneratedKind, Method};
use crate::util::{
    same_file, sanitize_file_name, unique_path, CancelToken, Job, Reservations, MIB,
};

/// Один файл пакета: исходник, путь результата и его целевой размер.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub input: PathBuf,
    pub output: PathBuf,
    pub target: u64,
}

impl BatchItem {
    /// Собирает элемент пакета, проверяя вход и подбирая свободное имя результата.
    pub fn new(
        input: &Path,
        target_bytes: u64,
        method: Method,
        format: AnimationFormat,
        out_dir: Option<&Path>,
        names: NameMode,
    ) -> Result<Self, String> {
        if !input.is_file() {
            return Err("файл не найден".to_owned());
        }
        let output = output_path(input, target_bytes, method, format, out_dir, names)?;
        if same_file(input, &output) {
            return Err("результат совпадает с исходным файлом".to_owned());
        }
        Ok(Self {
            input: input.to_path_buf(),
            output,
            target: target_bytes,
        })
    }

    /// Элемент с заранее известным путём результата (`--output`).
    pub fn with_output(input: PathBuf, output: PathBuf, target: u64) -> Self {
        Self {
            input,
            output,
            target,
        }
    }
}

/// Как понимать `--size`: точный размер, минимум или прибавка к исходному файлу.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetMode {
    /// Результат ровно такого размера (по умолчанию).
    #[default]
    Exact,
    /// Не меньше указанного: файл крупнее не уменьшается.
    AtLeast,
    /// Прибавить указанный объём к размеру исходного файла.
    Add,
}

impl TargetMode {
    pub const ALL: [TargetMode; 3] = [TargetMode::Exact, TargetMode::AtLeast, TargetMode::Add];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "exact" | "точно" => Ok(TargetMode::Exact),
            "at-least" | "atleast" | "min" | "не-меньше" => Ok(TargetMode::AtLeast),
            "add" | "прибавить" => Ok(TargetMode::Add),
            other => Err(format!(
                "Неизвестный режим размера: {other}. Допустимо: exact, at-least, add."
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TargetMode::Exact => "точно",
            TargetMode::AtLeast => "не меньше",
            TargetMode::Add => "прибавить",
        }
    }

    /// Описание режима для интерфейса.
    pub fn title(self) -> &'static str {
        match self {
            TargetMode::Exact => "Результат ровно указанного размера",
            TargetMode::AtLeast => "Не меньше указанного (крупный файл не уменьшается)",
            TargetMode::Add => "Прибавить указанный объём к исходному файлу",
        }
    }

    /// Целевой размер с учётом размера исходника.
    pub fn target_bytes(self, target: u64, input_len: u64) -> u64 {
        match self {
            TargetMode::Exact => target,
            TargetMode::AtLeast => target.max(input_len),
            TargetMode::Add => input_len.saturating_add(target),
        }
    }
}

/// Куда отклоняется размер при разбросе: только вверх, только вниз или в обе стороны.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JitterMode {
    /// Только прибавлять: 10 МБ + 0…2 МБ.
    Plus,
    /// Только убавлять: 10 МБ − 0…2 МБ.
    Minus,
    /// В любую сторону (по умолчанию): 10 ± 2 МБ.
    #[default]
    Both,
}

impl JitterMode {
    pub const ALL: [JitterMode; 3] = [JitterMode::Both, JitterMode::Plus, JitterMode::Minus];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "both" | "plus-minus" | "±" | "обе" | "в-обе-стороны" => {
                Ok(JitterMode::Both)
            }
            "plus" | "up" | "+" | "плюс" => Ok(JitterMode::Plus),
            "minus" | "down" | "-" | "минус" => Ok(JitterMode::Minus),
            other => Err(format!(
                "Неизвестное направление разброса: {other}. Допустимо: plus, minus, both."
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            JitterMode::Both => "плюс-минус",
            JitterMode::Plus => "только плюс",
            JitterMode::Minus => "только минус",
        }
    }

    /// Пояснение с примером — для интерфейса и справки.
    pub fn title(self) -> &'static str {
        match self {
            JitterMode::Both => "Плюс-минус: 10 ± 2 МБ — от 8 до 12 МБ",
            JitterMode::Plus => "Только плюс: 10 + 0…2 МБ — от 10 до 12 МБ",
            JitterMode::Minus => "Только минус: 10 − 0…2 МБ — от 8 до 10 МБ",
        }
    }
}

/// Разброс целевого размера: вместо ровных 10 МБ файл получает случайный размер
/// в пределах заданного отклонения.
///
/// Разброс применяется к уже посчитанному размеру (с учётом [`TargetMode`]),
/// а результат остаётся точным: файл создаётся ровно того размера, который выпал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeJitter {
    /// Максимальное отклонение в байтах (граница включительно).
    pub spread: u64,
    /// Куда отклоняться.
    pub mode: JitterMode,
}

impl SizeJitter {
    pub fn new(spread: u64, mode: JitterMode) -> Self {
        Self { spread, mode }
    }

    /// Разброс из мегабайт. `Ok(None)` — разброс выключен (`--vary 0`).
    pub fn from_mb(spread_mb: f64, mode: JitterMode) -> Result<Option<Self>, String> {
        if !spread_mb.is_finite() {
            return Err("Разброс должен быть числом в мегабайтах.".to_owned());
        }
        if spread_mb < 0.0 {
            return Err("Разброс не может быть отрицательным.".to_owned());
        }
        let spread = (spread_mb * MIB as f64) as u64;
        Ok((spread > 0).then_some(Self { spread, mode }))
    }

    pub fn is_active(&self) -> bool {
        self.spread > 0
    }

    /// Случайное отклонение в байтах: от 0 до `spread` включительно.
    fn random_delta(&self) -> u64 {
        use rand::Rng;
        rand::thread_rng().gen_range(0..=self.spread)
    }

    /// Применяет разброс к базовому размеру.
    ///
    /// Вниз размер не опускается ниже одного байта: файл нулевой длины не нужен
    /// ни одному методу.
    pub fn apply(&self, base: u64) -> u64 {
        if !self.is_active() {
            return base;
        }
        let delta = self.random_delta();
        match self.mode {
            JitterMode::Plus => base.saturating_add(delta),
            JitterMode::Minus => base.saturating_sub(delta).max(1),
            JitterMode::Both => {
                use rand::Rng;
                if rand::thread_rng().gen_bool(0.5) {
                    base.saturating_add(delta)
                } else {
                    base.saturating_sub(delta).max(1)
                }
            }
        }
    }

    /// Границы, в которые попадёт размер: `(минимум, максимум)`.
    pub fn range(&self, base: u64) -> (u64, u64) {
        let top = base.saturating_add(self.spread);
        let bottom = base.saturating_sub(self.spread).max(1);
        match self.mode {
            JitterMode::Plus => (base, top),
            JitterMode::Minus => (bottom, base),
            JitterMode::Both => (bottom, top),
        }
    }

    /// Подпись для интерфейса, плана и отчёта: `±2MB`, `+0…2MB`, `−0…2MB`.
    pub fn label(&self) -> String {
        let spread = size_label(self.spread);
        match self.mode {
            JitterMode::Both => format!("±{spread}"),
            JitterMode::Plus => format!("+0…{spread}"),
            JitterMode::Minus => format!("−0…{spread}"),
        }
    }
}

/// Как называется файл результата: с размером в имени, без него или случайно.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NameMode {
    /// `photo_10MB.png` — размер дописывается к имени исходника (по умолчанию).
    #[default]
    Size,
    /// `photo.png` — только имя исходника, без пометки размера.
    Plain,
    /// `quiet-river-4821.png` — случайное имя «в рамках приличия».
    Random,
}

impl NameMode {
    pub const ALL: [NameMode; 3] = [NameMode::Size, NameMode::Plain, NameMode::Random];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "size" | "with-size" | "размер" | "с-размером" => Ok(NameMode::Size),
            "plain" | "clean" | "no-size" | "без-размера" => Ok(NameMode::Plain),
            "random" | "случайно" | "случайное" => Ok(NameMode::Random),
            other => Err(format!(
                "Неизвестный режим имени: {other}. Допустимо: size, plain, random."
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NameMode::Size => "с размером",
            NameMode::Plain => "без размера",
            NameMode::Random => "случайное",
        }
    }

    /// Пример и пояснение режима для интерфейса и справки.
    pub fn title(self) -> &'static str {
        match self {
            NameMode::Size => "С размером: photo_10MB.png",
            NameMode::Plain => "Без размера: photo.png",
            NameMode::Random => "Случайное: quiet-river-4821.png",
        }
    }
}

/// Готовый план пакета: настройки, размеры и список входов.
#[derive(Clone, Debug)]
pub struct PumpPlan {
    pub method: Method,
    pub format: AnimationFormat,
    pub out_dir: Option<PathBuf>,
    pub mode: TargetMode,
    /// Как называть результаты пакета.
    pub names: NameMode,
    /// Разброс размера: `None` — размер всегда ровно указанный.
    pub jitter: Option<SizeJitter>,
    /// Размеры в байтах, по одному на файл; последний повторяется.
    pub targets: Vec<u64>,
}

impl PumpPlan {
    pub fn new(
        method: Method,
        format: AnimationFormat,
        out_dir: Option<PathBuf>,
        mode: TargetMode,
        sizes_mb: &[f64],
    ) -> Result<Self, String> {
        Ok(Self {
            method,
            format,
            out_dir,
            mode,
            names: NameMode::Size,
            jitter: None,
            targets: targets_from_mb(sizes_mb)?,
        })
    }

    /// Как называть результаты: с размером, без него или случайно.
    pub fn with_names(mut self, names: NameMode) -> Self {
        self.names = names;
        self
    }

    /// Разброс размера: каждому файлу достаётся случайный размер в его пределах.
    pub fn with_jitter(mut self, jitter: Option<SizeJitter>) -> Self {
        self.jitter = jitter;
        self
    }

    /// Размер для файла номер `index` (если размеров меньше — повторяется последний).
    pub fn target_for(&self, index: usize, input_len: u64) -> u64 {
        let base = self.targets[index.min(self.targets.len() - 1)];
        let exact = self.mode.target_bytes(base, input_len);
        match self.jitter {
            Some(jitter) => jitter.apply(exact),
            None => exact,
        }
    }

    /// Проверяет, что размеров столько же, сколько файлов, либо указан один.
    pub fn ensure_sizes_match(&self, inputs: usize) -> Result<(), String> {
        if self.targets.len() == 1 || self.targets.len() == inputs {
            return Ok(());
        }
        Err(format!(
            "Размеров указано {} ({}) — должно быть либо 1, либо столько же, сколько файлов ({}).",
            self.targets.len(),
            self.targets
                .iter()
                .map(|bytes| size_label(*bytes))
                .collect::<Vec<_>>()
                .join(", "),
            inputs
        ))
    }

    /// Строит элементы пакета: свободные имена результатов и отказы по каждому входу.
    /// Папки не создаются — это делает сам запуск, чтобы `--dry-run` ничего не менял.
    pub fn build(&self, inputs: &[PathBuf]) -> PlannedBatch {
        let mut reservations = Reservations::new();
        let mut planned = PlannedBatch::default();
        for (index, input) in inputs.iter().enumerate() {
            match self.build_one(input, index, &mut reservations) {
                Ok(item) => planned.items.push(item),
                Err(message) => planned.rejected.push((input.clone(), message)),
            }
        }
        planned
    }

    fn build_one(
        &self,
        input: &Path,
        index: usize,
        reservations: &mut Reservations,
    ) -> Result<BatchItem, String> {
        if !input.is_file() {
            return Err("файл не найден".to_owned());
        }
        let input_len = fs::metadata(input).map_err(|err| err.to_string())?.len();
        let target = self.target_for(index, input_len);
        let name = result_name(input, target, self.method, self.format, self.names);
        let candidate = match self.out_dir.as_deref() {
            Some(directory) => directory.join(&name),
            None => input.with_file_name(&name),
        };
        let output = reservations.reserve(&candidate);
        if same_file(input, &output) {
            return Err("результат совпадает с исходным файлом".to_owned());
        }
        Ok(BatchItem {
            input: input.to_path_buf(),
            output,
            target,
        })
    }
}

/// Итог планирования: что можно обрабатывать и что отклонено (с причиной).
#[derive(Clone, Debug, Default)]
pub struct PlannedBatch {
    pub items: Vec<BatchItem>,
    pub rejected: Vec<(PathBuf, String)>,
}

/// Причина, по которой метод не сможет обработать файл: нужна `--skip-unsupported`,
/// чтобы отсеять неподходящие файлы до запуска, а не после ошибки.
pub fn support_error(method: Method, input: &Path) -> Option<String> {
    match method {
        Method::Animation | Method::PngChunks | Method::Upscale => {
            let supported = input
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .is_some_and(|value| {
                    matches!(value.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
                });
            if supported {
                None
            } else {
                Some("метод работает только с PNG, JPEG, GIF и WebP".to_owned())
            }
        }
        Method::Svg | Method::OfficePdf | Method::Padding => None,
    }
}

/// Результат по одному файлу пакета.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemResult {
    Created { output: PathBuf, size: u64 },
    Failed { message: String },
    Skipped { reason: String },
}

/// Итог пакетного запуска.
#[derive(Clone, Debug, Default)]
pub struct BatchReport {
    pub items: Vec<(PathBuf, ItemResult)>,
    pub cancelled: bool,
}

impl BatchReport {
    pub fn created(&self) -> usize {
        self.items
            .iter()
            .filter(|(_, result)| matches!(result, ItemResult::Created { .. }))
            .count()
    }

    pub fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|(_, result)| matches!(result, ItemResult::Failed { .. }))
            .count()
    }

    pub fn skipped(&self) -> usize {
        self.items
            .iter()
            .filter(|(_, result)| matches!(result, ItemResult::Skipped { .. }))
            .count()
    }

    pub fn output_paths(&self) -> Vec<&Path> {
        self.items
            .iter()
            .filter_map(|(_, result)| match result {
                ItemResult::Created { output, .. } => Some(output.as_path()),
                _ => None,
            })
            .collect()
    }

    /// Пакет успешен, если ни один файл не упал и обработку не прервали.
    pub fn is_ok(&self) -> bool {
        !self.cancelled && self.failed() == 0
    }
}

/// Расширение результата для выбранного метода (как в графическом интерфейсе).
pub fn output_extension(method: Method, format: AnimationFormat, input: &Path) -> String {
    match method {
        Method::Animation => format.extension().to_owned(),
        Method::PngChunks | Method::Upscale => "png".to_owned(),
        Method::Svg => "svg".to_owned(),
        Method::OfficePdf | Method::Padding => input
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("bin")
            .to_owned(),
    }
}

/// Подпись размера для имени файла: `10MB`, `1.5MB`, `0.25MB`.
pub fn size_label(target_bytes: u64) -> String {
    let value = target_bytes as f64 / MIB as f64;
    if value >= 1.0 && (value - value.round()).abs() < 1e-9 {
        return format!("{}MB", value.round() as u64);
    }
    let mut text = format!("{value:.2}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{text}MB")
}

/// Имя результата: `photo.jpg` → `photo_10MB.png`. Расширение зависит от метода,
/// а [`NameMode`] решает, попадёт ли в имя размер (`Size`), исчезнет ли он (`Plain`)
/// или имя станет случайным (`Random`).
pub fn result_name(
    input: &Path,
    target_bytes: u64,
    method: Method,
    format: AnimationFormat,
    names: NameMode,
) -> String {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    let extension = output_extension(method, format, input);
    match names {
        NameMode::Size => format!("{stem}_{}.{extension}", size_label(target_bytes)),
        NameMode::Plain => format!("{stem}.{extension}"),
        NameMode::Random => format!("{}.{extension}", random_stem()),
    }
}

/// Прилагательные для случайных имён: нейтральные, короткие, латиницей —
/// чтобы имя одинаково выглядело в любой файловой системе.
const NAME_ADJECTIVES: [&str; 24] = [
    "amber", "brave", "bright", "calm", "clever", "cozy", "crisp", "gentle", "golden", "happy",
    "lucky", "merry", "misty", "quiet", "rapid", "silver", "sunny", "swift", "tidy", "velvet",
    "vivid", "warm", "wild", "witty",
];

/// Существительные для случайных имён.
const NAME_NOUNS: [&str; 24] = [
    "apple", "bay", "birch", "breeze", "brook", "cedar", "cloud", "comet", "dawn", "delta",
    "ember", "fern", "forest", "garden", "harbor", "hill", "island", "lagoon", "meadow", "moon",
    "orchard", "pebble", "river", "sky",
];

/// Случайная основа имени «в рамках приличия»: `quiet-river-4821`.
pub fn random_stem() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let adjective = NAME_ADJECTIVES[rng.gen_range(0..NAME_ADJECTIVES.len())];
    let noun = NAME_NOUNS[rng.gen_range(0..NAME_NOUNS.len())];
    let number: u32 = rng.gen_range(1000..10_000);
    format!("{adjective}-{noun}-{number}")
}

/// Подставляет случайную основу, сохраняя расширение:
/// `photo_with_payload.png` → `calm-bay-7314.png`.
pub fn with_random_stem(path: &Path) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("bin");
    path.with_file_name(format!("{}.{extension}", random_stem()))
}

/// Путь результата: рядом с исходником или в указанной папке, без перезаписи
/// существующих файлов.
pub fn output_path(
    input: &Path,
    target_bytes: u64,
    method: Method,
    format: AnimationFormat,
    out_dir: Option<&Path>,
    names: NameMode,
) -> Result<PathBuf, String> {
    let name = result_name(input, target_bytes, method, format, names);
    let candidate = match out_dir {
        Some(directory) => directory.join(&name),
        None => input.with_file_name(&name),
    };
    if let Some(directory) = candidate.parent() {
        if !directory.as_os_str().is_empty() && !directory.is_dir() {
            fs::create_dir_all(directory).map_err(|err| {
                format!("Не удалось создать папку {}: {err}", directory.display())
            })?;
        }
    }
    Ok(unique_path(&candidate))
}

/// Обрабатывает один файл пакета и возвращает размер результата.
pub fn pump_one(
    item: &BatchItem,
    method: Method,
    format: AnimationFormat,
    target_bytes: u64,
    job: &Job<'_>,
) -> Result<u64, String> {
    job.check()?;
    if same_file(&item.input, &item.output) {
        return Err("результат совпадает с исходным файлом".to_owned());
    }
    // Папку результата создаём здесь, а не на этапе плана: `--dry-run` не должен
    // оставлять после себя пустых папок.
    if let Some(directory) = item.output.parent() {
        if !directory.as_os_str().is_empty() && !directory.is_dir() {
            fs::create_dir_all(directory).map_err(|err| {
                format!("Не удалось создать папку {}: {err}", directory.display())
            })?;
        }
    }
    pumper::run(method, format, &item.input, &item.output, target_bytes, job)?;
    fs::metadata(&item.output)
        .map(|metadata| metadata.len())
        .map_err(|err| err.to_string())
}

/// Ошибка отмены отдельного файла превращается в «пропущено».
fn failing_result(message: String) -> (ItemResult, bool) {
    if message == crate::util::CANCELLED_MESSAGE {
        (
            ItemResult::Skipped {
                reason: crate::util::CANCELLED_MESSAGE.to_owned(),
            },
            true,
        )
    } else {
        (ItemResult::Failed { message }, false)
    }
}

/// Последовательно обрабатывает весь пакет.
///
/// Ошибка отдельного файла не прерывает пакет: она попадает в отчёт, обработка
/// продолжается. Отмена прерывает пакет, недоделанные файлы помечаются пропущенными.
/// Целевой размер берётся из каждого элемента (`BatchItem::target`).
pub fn pump(
    items: &[BatchItem],
    method: Method,
    format: AnimationFormat,
    job: &Job<'_>,
) -> BatchReport {
    let total = items.len().max(1) as f32;
    let mut report = BatchReport::default();

    for (index, item) in items.iter().enumerate() {
        if report.cancelled || job.check().is_err() {
            report.cancelled = true;
            report.items.push((
                item.input.clone(),
                ItemResult::Skipped {
                    reason: crate::util::CANCELLED_MESSAGE.to_owned(),
                },
            ));
            continue;
        }
        let progress = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            job.report(overall, text);
        };
        let item_job = job.child(&progress);
        let result = match pump_one(item, method, format, item.target, &item_job) {
            Ok(size) => ItemResult::Created {
                output: item.output.clone(),
                size,
            },
            Err(message) => {
                let (result, cancelled) = failing_result(message);
                report.cancelled |= cancelled;
                result
            }
        };
        report.items.push((item.input.clone(), result));
    }

    report
}

/// Параллельный прогон: `jobs` потоков берут файлы из общей очереди.
///
/// Прогресс складывается из долей всех файлов, поэтому общая полоса остаётся
/// монотонной. Порядок в отчёте совпадает с порядком входов, а отмена
/// останавливает очередь и помечает необработанные файлы пропущенными.
pub fn pump_parallel(
    items: &[BatchItem],
    method: Method,
    format: AnimationFormat,
    jobs: usize,
    progress: &(dyn Fn(f32, &str) + Sync),
    cancel: &CancelToken,
) -> BatchReport {
    let jobs = jobs.clamp(1, 16).min(items.len().max(1));
    if jobs <= 1 {
        let job = Job::new(progress, cancel);
        return pump(items, method, format, &job);
    }

    let cursor = AtomicUsize::new(0);
    let fractions = Mutex::new(vec![0.0_f32; items.len()]);
    let results: Mutex<Vec<Option<ItemResult>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());
    let total = items.len() as f32;

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let index = cursor.fetch_add(1, Ordering::SeqCst);
                if index >= items.len() {
                    break;
                }
                let item = &items[index];
                if cancel.is_cancelled() {
                    results.lock().unwrap()[index] = Some(ItemResult::Skipped {
                        reason: crate::util::CANCELLED_MESSAGE.to_owned(),
                    });
                    continue;
                }
                let local = |percent: f32, text: &str| {
                    let overall = {
                        let mut guard = fractions.lock().unwrap();
                        guard[index] = percent.clamp(0.0, 100.0) / 100.0;
                        guard.iter().sum::<f32>() / total * 100.0
                    };
                    progress(overall, text);
                };
                let job = Job::new(&local, cancel);
                let result = match pump_one(item, method, format, item.target, &job) {
                    Ok(size) => ItemResult::Created {
                        output: item.output.clone(),
                        size,
                    },
                    Err(message) => failing_result(message).0,
                };
                results.lock().unwrap()[index] = Some(result);
            });
        }
    });

    let mut cancelled = cancel.is_cancelled();
    let mut report = BatchReport::default();
    for (item, result) in items.iter().zip(results.into_inner().unwrap()) {
        let result = result.unwrap_or(ItemResult::Skipped {
            reason: "файл не обработан".to_owned(),
        });
        cancelled |= matches!(result, ItemResult::Skipped { .. });
        report.items.push((item.input.clone(), result));
    }
    report.cancelled = cancelled;
    report
}

/// Размеры в байтах из списка мегабайт: `[10.0, 0.5]` → `[10485760, 524288]`.
pub fn targets_from_mb(sizes_mb: &[f64]) -> Result<Vec<u64>, String> {
    if sizes_mb.is_empty() {
        return Err("Укажите размер: --size <МБ>.".to_owned());
    }
    let mut targets = Vec::with_capacity(sizes_mb.len());
    for size_mb in sizes_mb {
        if !(size_mb.is_finite() && *size_mb > 0.0) {
            return Err("Размер должен быть положительным числом.".to_owned());
        }
        let target = (size_mb * MIB as f64) as u64;
        if target == 0 {
            return Err("Размер слишком мал: укажите хотя бы 0,001 МБ.".to_owned());
        }
        targets.push(target);
    }
    Ok(targets)
}

/// Основа имени для созданного файла: пустая строка превращается в `file`,
/// недопустимые символы вырезаются, хвостовые точки и пробелы убираются.
fn generated_stem(base: &str) -> String {
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return "file".to_owned();
    }
    let cleaned = sanitize_file_name(trimmed);
    let stem = cleaned.trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        "file".to_owned()
    } else {
        stem.to_owned()
    }
}

/// Имя файла, созданного «с нуля»: `file_10MB.bin`, `file.png` или
/// `quiet-river-4821.png` — режим имён тот же, что у накачки.
pub fn generated_name(
    base: &str,
    target_bytes: u64,
    kind: GeneratedKind,
    names: NameMode,
) -> String {
    let stem = generated_stem(base);
    let extension = kind.extension();
    match names {
        NameMode::Size => format!("{stem}_{}.{extension}", size_label(target_bytes)),
        NameMode::Plain => format!("{stem}.{extension}"),
        NameMode::Random => format!("{}.{extension}", random_stem()),
    }
}

/// Путь для файла, созданного «с нуля»: папка результатов или текущая,
/// без перезаписи существующих файлов.
pub fn generated_path(
    base: &str,
    target_bytes: u64,
    kind: GeneratedKind,
    out_dir: Option<&Path>,
    names: NameMode,
) -> Result<PathBuf, String> {
    let mut reservations = Reservations::new();
    generated_path_in(base, target_bytes, kind, out_dir, names, &mut reservations)
}

/// Общий набор занятых имён нужен серии файлов: два одинаковых размера не
/// должны получить один и тот же путь.
fn generated_path_in(
    base: &str,
    target_bytes: u64,
    kind: GeneratedKind,
    out_dir: Option<&Path>,
    names: NameMode,
    reservations: &mut Reservations,
) -> Result<PathBuf, String> {
    let name = generated_name(base, target_bytes, kind, names);
    let candidate = match out_dir {
        Some(directory) => directory.join(name),
        None => PathBuf::from(name),
    };
    Ok(reservations.reserve(&candidate))
}

/// Задание на создание файла «с нуля»: путь результата, размер, тип и
/// (необязательно) нагрузка для второго дна.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateItem {
    pub output: PathBuf,
    pub target: u64,
    pub kind: GeneratedKind,
    pub payloads: Vec<PathBuf>,
}

/// Что создавать «с нуля»: размеры в байтах, число повторов и разброс размера.
#[derive(Clone, Debug, Default)]
pub struct GenerateTargets {
    /// Размеры в байтах; каждый повторяется `count` раз.
    pub sizes: Vec<u64>,
    /// Сколько файлов создать на каждый размер.
    pub count: usize,
    /// Разброс размера: `None` — размер всегда ровно указанный.
    pub jitter: Option<SizeJitter>,
}

/// План генерации: каждый размер создаётся `count` раз, имена — по [`NameMode`].
pub fn plan_generate(
    base: &str,
    kind: GeneratedKind,
    names: NameMode,
    out_dir: Option<&Path>,
    targets: &GenerateTargets,
    payloads: &[PathBuf],
) -> Result<Vec<GenerateItem>, String> {
    if targets.sizes.is_empty() {
        return Err("Укажите размер файла.".to_owned());
    }
    let count = targets.count.clamp(1, 64);
    let mut reservations = Reservations::new();
    let mut items = Vec::with_capacity(targets.sizes.len() * count);
    for size in &targets.sizes {
        for _ in 0..count {
            // Разброс считается для каждого файла отдельно: у серии из `count`
            // файлов размеры будут разными, и имя покажет фактический размер.
            let exact = match targets.jitter {
                Some(jitter) => jitter.apply(*size),
                None => *size,
            };
            let output = generated_path_in(base, exact, kind, out_dir, names, &mut reservations)?;
            items.push(GenerateItem {
                output,
                target: exact,
                kind,
                payloads: payloads.to_vec(),
            });
        }
    }
    Ok(items)
}

/// Создаёт файл «с нуля» и возвращает его размер.
///
/// Если указана нагрузка, она прячется внутрь готового файла (второе дно):
/// размер остаётся ровно целевым, потому что контейнер встраивается в заранее
/// посчитанное место, а не дописывается сверху.
pub fn generate_one(
    item: &GenerateItem,
    options: &ContainerOptions,
    copies: usize,
    job: &Job<'_>,
) -> Result<u64, String> {
    job.check()?;
    create_parent_dir(&item.output)?;
    if item.payloads.is_empty() {
        return generate::run(item.kind, &item.output, item.target, job);
    }

    let copies = copies.clamp(payload::MIN_COPIES, payload::MAX_COPIES);
    let prepared = payload::prepare_payload(&item.payloads, job)?;
    let mut options = options.clone();
    options.bundle = prepared.bundle;
    let container_len = prepared.container_len(&options, copies, job)?;
    let min_size = generate::min_size(item.kind)?;
    if item.target < min_size + container_len {
        return Err(too_small_message(item.kind, min_size, container_len));
    }

    // Служебные байты формата (заголовки чанков, сегментов, блоков) измеряем
    // на минимальном носителе: тогда готовый файл попадёт точно в цель.
    let overhead = measure_overhead(item.kind, &prepared, &options, copies, &item.output, job)?;
    let carrier_target = item.target - container_len - overhead;
    if carrier_target < min_size {
        return Err(too_small_message(
            item.kind,
            min_size,
            container_len + overhead,
        ));
    }

    job.report(5.0, "Генерация случайных данных...");
    let directory = parent_dir(&item.output).to_path_buf();
    let carrier = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|err| format!("Не удалось создать временный файл: {err}"))?;
    generate::run(item.kind, carrier.path(), carrier_target, job)?;
    payload::embed_bytes(
        carrier.path(),
        &prepared.bytes,
        &prepared.name,
        &item.output,
        &options,
        copies,
        job,
    )?;
    drop(carrier);

    let len = fs::metadata(&item.output)
        .map_err(|err| err.to_string())?
        .len();
    if len != item.target {
        return Err(format!(
            "Ожидался размер {} байт, получилось {}.",
            item.target, len
        ));
    }
    job.report(100.0, "Готово.");
    Ok(len)
}

/// Сколько байт формат добавляет вокруг контейнера: узнаём на минимальном
/// носителе, чтобы не считать формулы для PNG, GIF, JPEG и WebP вручную.
fn measure_overhead(
    kind: GeneratedKind,
    prepared: &payload::PreparedPayload,
    options: &ContainerOptions,
    copies: usize,
    output: &Path,
    job: &Job<'_>,
) -> Result<u64, String> {
    let directory = parent_dir(output);
    let base = generate::base_bytes(kind)?;
    let carrier = tempfile::NamedTempFile::new_in(directory)
        .map_err(|err| format!("Не удалось создать временный файл: {err}"))?;
    fs::write(carrier.path(), &base).map_err(|err| err.to_string())?;
    let probe = tempfile::NamedTempFile::new_in(directory)
        .map_err(|err| format!("Не удалось создать временный файл: {err}"))?;
    payload::embed_bytes(
        carrier.path(),
        &prepared.bytes,
        &prepared.name,
        probe.path(),
        options,
        copies,
        job,
    )?;
    let container_len = prepared.container_len(options, copies, job)?;
    let probe_len = fs::metadata(probe.path())
        .map_err(|err| err.to_string())?
        .len();
    drop(probe);
    drop(carrier);
    Ok(probe_len.saturating_sub(base.len() as u64 + container_len))
}

fn too_small_message(kind: GeneratedKind, min_size: u64, payload_len: u64) -> String {
    format!(
        "Размер слишком мал для типа «{}»: нужно минимум {} байт (служебные данные {} + второе дно {}).",
        kind.title(),
        min_size.saturating_add(payload_len),
        min_size,
        payload_len
    )
}

/// Папка результата: пустой родитель означает текущую папку.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory,
        _ => Path::new("."),
    }
}

fn create_parent_dir(path: &Path) -> Result<(), String> {
    let directory = parent_dir(path);
    if !directory.is_dir() {
        fs::create_dir_all(directory)
            .map_err(|err| format!("Не удалось создать папку {}: {err}", directory.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_label_is_readable() {
        assert_eq!(size_label(10 * MIB), "10MB");
        assert_eq!(size_label(0), "0MB");
        assert_eq!(size_label(MIB / 4), "0.25MB");
        assert_eq!(size_label(MIB), "1MB");
    }

    #[test]
    fn result_name_uses_method_extension() {
        let input = Path::new("C:/photos/IMG_0001.jpeg");
        assert_eq!(
            result_name(
                input,
                5 * MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                NameMode::Size
            ),
            "IMG_0001_5MB.png"
        );
        assert_eq!(
            result_name(
                input,
                5 * MIB,
                Method::Animation,
                AnimationFormat::Webp,
                NameMode::Size
            ),
            "IMG_0001_5MB.webp"
        );
        assert_eq!(
            result_name(
                input,
                5 * MIB,
                Method::Padding,
                AnimationFormat::Gif,
                NameMode::Size
            ),
            "IMG_0001_5MB.jpeg"
        );
        assert_eq!(
            result_name(
                input,
                5 * MIB,
                Method::Svg,
                AnimationFormat::Gif,
                NameMode::Size
            ),
            "IMG_0001_5MB.svg"
        );
    }

    #[test]
    fn name_mode_parse_accepts_russians() {
        assert_eq!(NameMode::parse("size"), Ok(NameMode::Size));
        assert_eq!(NameMode::parse("plain"), Ok(NameMode::Plain));
        assert_eq!(NameMode::parse("no-size"), Ok(NameMode::Plain));
        assert_eq!(NameMode::parse(" Random "), Ok(NameMode::Random));
        assert_eq!(NameMode::parse("случайное"), Ok(NameMode::Random));
        assert!(NameMode::parse("мусор").is_err());
        assert_eq!(NameMode::default(), NameMode::Size);
    }

    #[test]
    fn plain_mode_drops_the_size_label() {
        let input = Path::new("C:/photos/photo.jpeg");
        assert_eq!(
            result_name(
                input,
                10 * MIB,
                Method::Padding,
                AnimationFormat::Gif,
                NameMode::Plain
            ),
            "photo.jpeg"
        );
        assert_eq!(
            result_name(
                input,
                MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                NameMode::Plain
            ),
            "photo.png"
        );
        assert_eq!(
            result_name(
                input,
                MIB,
                Method::Svg,
                AnimationFormat::Gif,
                NameMode::Plain
            ),
            "photo.svg"
        );
    }

    #[test]
    fn random_mode_generates_tidy_names() {
        let input = Path::new("C:/photos/photo.jpeg");
        for _ in 0..50 {
            let name = result_name(
                input,
                10 * MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                NameMode::Random,
            );
            let stem = name
                .strip_suffix(".png")
                .unwrap_or_else(|| panic!("нет расширения: {name}"));
            assert!(is_tidy_random_stem(stem), "неприличное имя: {name}");
        }
        let renamed = with_random_stem(Path::new("C:/photos/photo_with_payload.png"));
        assert_eq!(renamed.extension().and_then(|v| v.to_str()), Some("png"));
        let stem = renamed.file_stem().and_then(|v| v.to_str()).unwrap();
        assert!(is_tidy_random_stem(stem), "имя: {stem}");
    }

    /// `quiet-river-4821`: два слова из списков и число от 1000 до 9999.
    fn is_tidy_random_stem(stem: &str) -> bool {
        let parts: Vec<&str> = stem.split('-').collect();
        parts.len() == 3
            && NAME_ADJECTIVES.contains(&parts[0])
            && NAME_NOUNS.contains(&parts[1])
            && parts[2].len() == 4
            && parts[2]
                .parse::<u32>()
                .is_ok_and(|value| (1000..10_000).contains(&value))
    }

    #[test]
    fn output_path_never_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.png");
        fs::write(&input, b"x").unwrap();
        let first = output_path(
            &input,
            MIB,
            Method::Padding,
            AnimationFormat::Gif,
            None,
            NameMode::Size,
        )
        .unwrap();
        assert_eq!(first, dir.path().join("photo_1MB.png"));
        fs::write(&first, b"y").unwrap();
        let second = output_path(
            &input,
            MIB,
            Method::Padding,
            AnimationFormat::Gif,
            None,
            NameMode::Size,
        )
        .unwrap();
        assert_eq!(second, dir.path().join("photo_1MB-2.png"));
    }

    #[test]
    fn plain_mode_next_to_the_source_picks_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.png");
        fs::write(&input, b"x").unwrap();
        let output = output_path(
            &input,
            MIB,
            Method::Padding,
            AnimationFormat::Gif,
            None,
            NameMode::Plain,
        )
        .unwrap();
        // Исходник остаётся нетронутым: имя получает суффикс, а не совпадает с ним.
        assert_eq!(output, dir.path().join("photo-2.png"));
    }

    #[test]
    fn batch_creates_every_file_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        fs::write(&first, b"hello").unwrap();
        fs::write(&second, b"world").unwrap();
        let out_dir = dir.path().join("out");

        let items = vec![
            BatchItem::new(
                &first,
                MIB,
                Method::Padding,
                AnimationFormat::Gif,
                Some(&out_dir),
                NameMode::Size,
            )
            .unwrap(),
            BatchItem::new(
                &second,
                MIB,
                Method::Padding,
                AnimationFormat::Gif,
                Some(&out_dir),
                NameMode::Size,
            )
            .unwrap(),
        ];
        let job = Job::quiet();
        let report = pump(&items, Method::Padding, AnimationFormat::Gif, &job);
        assert_eq!(report.created(), 2);
        assert_eq!(report.failed(), 0);
        assert!(report.is_ok());
        for path in report.output_paths() {
            assert_eq!(fs::metadata(path).unwrap().len(), MIB);
        }

        let broken = vec![
            BatchItem::with_output(first, out_dir.join("a.bin"), MIB),
            BatchItem::with_output(dir.path().join("missing.txt"), out_dir.join("b.bin"), MIB),
        ];
        let report = pump(&broken, Method::Padding, AnimationFormat::Gif, &job);
        assert_eq!(report.created(), 1);
        assert_eq!(report.failed(), 1);
        assert!(!report.is_ok());
    }

    #[test]
    fn image_method_failure_does_not_stop_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("note.txt");
        let png = dir.path().join("picture.png");
        fs::write(&text, b"definitely not an image").unwrap();
        image::RgbImage::from_fn(4, 4, |x, y| image::Rgb([x as u8, y as u8, 7]))
            .save(&png)
            .unwrap();
        let out_dir = dir.path().join("out");

        let items = vec![
            BatchItem::new(
                &text,
                MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                Some(&out_dir),
                NameMode::Size,
            )
            .unwrap(),
            BatchItem::new(
                &png,
                MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                Some(&out_dir),
                NameMode::Size,
            )
            .unwrap(),
        ];
        let token = crate::util::CancelToken::new();
        let progress = |_: f32, _: &str| {};
        let job = Job::new(&progress, &token);
        let report = pump(&items, Method::PngChunks, AnimationFormat::Gif, &job);

        assert_eq!(report.failed(), 1);
        assert_eq!(report.created(), 1);
        assert!(matches!(report.items[0].1, ItemResult::Failed { .. }));
        assert!(matches!(report.items[1].1, ItemResult::Created { .. }));
    }

    #[test]
    fn cancelled_job_stops_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("file.txt");
        fs::write(&input, b"data").unwrap();
        let items = vec![
            BatchItem::with_output(input.clone(), dir.path().join("one.bin"), MIB),
            BatchItem::with_output(input, dir.path().join("two.bin"), MIB),
        ];
        let token = crate::util::CancelToken::new();
        token.cancel();
        let progress = |_: f32, _: &str| {};
        let job = Job::new(&progress, &token);
        let report = pump(&items, Method::Padding, AnimationFormat::Gif, &job);
        assert!(report.cancelled);
        assert_eq!(report.created(), 0);
        assert!(matches!(report.items[0].1, ItemResult::Skipped { .. }));
    }

    #[test]
    fn batch_item_rejects_missing_input() {
        let dir = tempfile::tempdir().unwrap();
        let error = BatchItem::new(
            &dir.path().join("nope.txt"),
            MIB,
            Method::Padding,
            AnimationFormat::Gif,
            None,
            NameMode::Size,
        )
        .unwrap_err();
        assert_eq!(error, "файл не найден");
    }

    #[test]
    fn target_modes_scale_with_input_size() {
        assert_eq!(TargetMode::Exact.target_bytes(MIB, 10 * MIB), MIB);
        assert_eq!(TargetMode::AtLeast.target_bytes(MIB, 10 * MIB), 10 * MIB);
        assert_eq!(TargetMode::AtLeast.target_bytes(20 * MIB, MIB), 20 * MIB);
        assert_eq!(TargetMode::Add.target_bytes(MIB, 10 * MIB), 11 * MIB);
        assert_eq!(TargetMode::parse("at-least").unwrap(), TargetMode::AtLeast);
        assert_eq!(TargetMode::parse("ADD").unwrap(), TargetMode::Add);
        assert!(TargetMode::parse("huge").is_err());
    }

    #[test]
    fn plan_uses_one_target_per_file_and_never_shares_names() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let first = dir.path().join("same.txt");
        let second = nested.join("same.txt");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let out_dir = dir.path().join("out");

        let plan = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(out_dir.clone()),
            TargetMode::Exact,
            &[1.0, 2.0],
        )
        .unwrap();
        let planned = plan.build(&[first.clone(), second.clone()]);

        assert!(planned.rejected.is_empty());
        assert_eq!(planned.items[0].target, MIB);
        assert_eq!(planned.items[1].target, 2 * MIB);
        assert_eq!(planned.items[0].output, out_dir.join("same_1MB.txt"));
        assert_eq!(planned.items[1].output, out_dir.join("same_2MB.txt"));

        let same_target = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(out_dir.clone()),
            TargetMode::Add,
            &[1.0],
        )
        .unwrap()
        .build(&[first.clone(), second.clone()]);
        assert_eq!(same_target.items[0].target, MIB + 1);
        assert_ne!(same_target.items[0].output, same_target.items[1].output);
    }

    #[test]
    fn plan_applies_name_mode_to_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let out_dir = dir.path().join("out");

        let plain = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(out_dir.clone()),
            TargetMode::Exact,
            &[1.0],
        )
        .unwrap()
        .with_names(NameMode::Plain)
        .build(&[first.clone(), second.clone()]);
        assert_eq!(plain.items[0].output, out_dir.join("first.txt"));
        assert_eq!(plain.items[1].output, out_dir.join("second.txt"));

        let random = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(out_dir.clone()),
            TargetMode::Exact,
            &[1.0],
        )
        .unwrap()
        .with_names(NameMode::Random)
        .build(&[first.clone(), second.clone()]);
        assert!(random.rejected.is_empty());
        // Случайные имена не должны ни совпадать, ни содержать размер.
        assert_ne!(random.items[0].output, random.items[1].output);
        for item in &random.items {
            let stem = item
                .output
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap();
            assert!(is_tidy_random_stem(stem), "имя: {stem}");
            assert!(!stem.contains("MB"), "в имени остался размер: {stem}");
        }
    }

    #[test]
    fn sizes_must_be_single_or_match_input_count() {
        let plan = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            None,
            TargetMode::Exact,
            &[1.0, 2.0],
        )
        .unwrap();
        assert!(plan.ensure_sizes_match(2).is_ok());
        assert!(plan.ensure_sizes_match(3).is_err());
    }

    #[test]
    fn unsupported_files_are_recognised_before_running() {
        assert!(support_error(Method::PngChunks, Path::new("note.txt")).is_some());
        assert!(support_error(Method::PngChunks, Path::new("photo.PNG")).is_none());
        assert!(support_error(Method::Padding, Path::new("note.txt")).is_none());
        assert!(support_error(Method::Svg, Path::new("note.txt")).is_none());
    }

    #[test]
    fn parallel_run_produces_every_file_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("out");
        let mut inputs = Vec::new();
        for index in 0..6 {
            let path = dir.path().join(format!("file{index}.txt"));
            fs::write(&path, format!("data {index}")).unwrap();
            inputs.push(path);
        }
        let plan = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(out_dir.clone()),
            TargetMode::Exact,
            &[1.0],
        )
        .unwrap();
        let planned = plan.build(&inputs);
        let token = CancelToken::new();
        let progress = |_: f32, _: &str| {};
        let report = pump_parallel(
            &planned.items,
            Method::Padding,
            AnimationFormat::Gif,
            3,
            &progress,
            &token,
        );

        assert_eq!(report.created(), 6);
        assert_eq!(report.failed(), 0);
        assert!(report.is_ok());
        let names: Vec<&Path> = report.output_paths();
        assert_eq!(names.len(), 6);
        for (index, name) in names.iter().enumerate() {
            assert_eq!(*name, out_dir.join(format!("file{index}_1MB.txt")));
            assert_eq!(fs::metadata(name).unwrap().len(), MIB);
        }
    }

    #[test]
    fn parallel_run_honours_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("file.txt");
        fs::write(&input, b"data").unwrap();
        let items = vec![
            BatchItem::with_output(input.clone(), dir.path().join("one.bin"), MIB),
            BatchItem::with_output(input, dir.path().join("two.bin"), MIB),
        ];
        let token = CancelToken::new();
        token.cancel();
        let progress = |_: f32, _: &str| {};
        let report = pump_parallel(
            &items,
            Method::Padding,
            AnimationFormat::Gif,
            2,
            &progress,
            &token,
        );
        assert!(report.cancelled);
        assert_eq!(report.created(), 0);
        assert_eq!(report.skipped(), 2);
    }

    #[test]
    fn plan_generate_repeats_every_size_and_keeps_names_unique() {
        let dir = tempfile::tempdir().unwrap();
        let targets = GenerateTargets {
            sizes: vec![MIB, MIB / 2],
            count: 2,
            jitter: None,
        };
        let items = plan_generate(
            "seed",
            GeneratedKind::Bin,
            NameMode::Size,
            Some(dir.path()),
            &targets,
            &[],
        )
        .unwrap();

        assert_eq!(items.len(), 4);
        let names: Vec<PathBuf> = items.iter().map(|item| item.output.clone()).collect();
        assert_eq!(names[0], dir.path().join("seed_1MB.bin"));
        assert_eq!(names[1], dir.path().join("seed_1MB-2.bin"));
        assert_eq!(names[2], dir.path().join("seed_0.5MB.bin"));
        assert_eq!(names[3], dir.path().join("seed_0.5MB-2.bin"));
        assert_eq!(items[0].target, MIB);
        assert_eq!(items[2].target, MIB / 2);
        assert!(items.iter().all(|item| item.payloads.is_empty()));
    }

    #[test]
    fn jitter_mode_parses_and_describes_itself() {
        assert_eq!(JitterMode::parse("plus").unwrap(), JitterMode::Plus);
        assert_eq!(JitterMode::parse("MINUS").unwrap(), JitterMode::Minus);
        assert_eq!(JitterMode::parse(" both ").unwrap(), JitterMode::Both);
        assert_eq!(JitterMode::parse("±").unwrap(), JitterMode::Both);
        assert!(JitterMode::parse("sideways").is_err());
        assert_eq!(JitterMode::default(), JitterMode::Both);
        for mode in JitterMode::ALL {
            assert!(!mode.label().is_empty());
            assert!(!mode.title().is_empty());
        }
    }

    #[test]
    fn jitter_from_mb_validates_the_spread() {
        assert!(SizeJitter::from_mb(0.0, JitterMode::Both)
            .unwrap()
            .is_none());
        assert!(SizeJitter::from_mb(0.5, JitterMode::Both)
            .unwrap()
            .is_some());
        assert_eq!(
            SizeJitter::from_mb(2.0, JitterMode::Plus).unwrap().unwrap(),
            SizeJitter::new(2 * MIB, JitterMode::Plus)
        );
        assert!(SizeJitter::from_mb(-1.0, JitterMode::Both).is_err());
        assert!(SizeJitter::from_mb(f64::NAN, JitterMode::Both).is_err());
    }

    #[test]
    fn jitter_label_shows_the_direction() {
        let spread = 2 * MIB;
        assert_eq!(SizeJitter::new(spread, JitterMode::Both).label(), "±2MB");
        assert_eq!(SizeJitter::new(spread, JitterMode::Plus).label(), "+0…2MB");
        assert_eq!(SizeJitter::new(spread, JitterMode::Minus).label(), "−0…2MB");
    }

    fn base_size() -> u64 {
        3 * MIB
    }

    #[test]
    fn jitter_stays_inside_the_range_and_follows_the_direction() {
        let base = 8 * MIB;
        let spread = MIB;
        let plus = SizeJitter::new(spread, JitterMode::Plus);
        let minus = SizeJitter::new(spread, JitterMode::Minus);
        let both = SizeJitter::new(spread, JitterMode::Both);

        let mut saw_above = false;
        let mut saw_below = false;
        for _ in 0..200 {
            let value = plus.apply(base);
            assert!((base..=base + spread).contains(&value), "плюс: {value}");
            let value = minus.apply(base);
            assert!((base - spread..=base).contains(&value), "минус: {value}");
            let value = both.apply(base);
            assert!(
                (base - spread..=base + spread).contains(&value),
                "плюс-минус: {value}"
            );
            saw_above |= value > base;
            saw_below |= value < base;
        }
        assert!(saw_above, "разброс «плюс-минус» не поднимался выше размера");
        assert!(saw_below, "разброс «плюс-минус» не опускался ниже размера");
        assert_eq!(both.range(base), (base - spread, base + spread));
        assert_eq!(plus.range(base), (base, base + spread));
        assert_eq!(minus.range(base), (base - spread, base));
    }

    #[test]
    fn jitter_never_produces_an_empty_file() {
        let minus = SizeJitter::new(5 * MIB, JitterMode::Minus);
        for _ in 0..50 {
            assert!(minus.apply(MIB) >= 1);
        }
        let both = SizeJitter::new(5 * MIB, JitterMode::Both);
        for _ in 0..50 {
            assert!(both.apply(MIB) >= 1);
        }
        assert_eq!(both.range(MIB).0, 1);
        let disabled = SizeJitter::new(0, JitterMode::Both);
        assert_eq!(disabled.apply(base_size()), base_size());
    }

    #[test]
    fn pump_plan_applies_jitter_to_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<PathBuf> = (0..4)
            .map(|index| {
                let path = dir.path().join(format!("file{index}.txt"));
                fs::write(&path, b"data").unwrap();
                path
            })
            .collect();
        let spread = MIB;
        let jitter = SizeJitter::new(spread, JitterMode::Both);
        let plan = PumpPlan::new(
            Method::Padding,
            AnimationFormat::Gif,
            Some(dir.path().join("out")),
            TargetMode::Exact,
            &[4.0],
        )
        .unwrap()
        .with_jitter(Some(jitter));
        let planned = plan.build(&inputs);

        assert_eq!(planned.items.len(), 4);
        for item in &planned.items {
            assert!(
                (4 * MIB - spread..=4 * MIB + spread).contains(&item.target),
                "размер {} вне разброса",
                item.target
            );
        }
    }

    #[test]
    fn generate_plan_gives_every_file_its_own_random_size() {
        let dir = tempfile::tempdir().unwrap();
        let spread = MIB / 4;
        let targets = GenerateTargets {
            sizes: vec![2 * MIB],
            count: 6,
            jitter: Some(SizeJitter::new(spread, JitterMode::Plus)),
        };
        let items = plan_generate(
            "seed",
            GeneratedKind::Bin,
            NameMode::Size,
            Some(dir.path()),
            &targets,
            &[],
        )
        .unwrap();

        assert_eq!(items.len(), 6);
        let mut distinct = std::collections::HashSet::new();
        for item in &items {
            assert!(
                (2 * MIB..=2 * MIB + spread).contains(&item.target),
                "размер {} вне разброса",
                item.target
            );
            distinct.insert(item.target);
        }
        assert!(distinct.len() > 1, "размеры серии совпали: {distinct:?}");
    }

    #[test]
    fn generate_one_writes_exactly_the_requested_size() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("made.bin");
        let item = GenerateItem {
            output: output.clone(),
            target: 64 * 1024,
            kind: GeneratedKind::Bin,
            payloads: Vec::new(),
        };
        let written = generate_one(
            &item,
            &ContainerOptions::default(),
            payload::MIN_COPIES,
            &Job::quiet(),
        )
        .unwrap();
        assert_eq!(written, item.target);
        assert_eq!(fs::metadata(&output).unwrap().len(), item.target);
    }

    #[test]
    fn generate_one_hides_payload_without_growing_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        fs::write(&secret, b"hidden payload\n").unwrap();
        let output = dir.path().join("made.png");
        let item = GenerateItem {
            output: output.clone(),
            target: 256 * 1024,
            kind: GeneratedKind::Png,
            payloads: vec![secret],
        };
        let options = ContainerOptions::default();
        let written = generate_one(&item, &options, payload::MIN_COPIES, &Job::quiet()).unwrap();

        assert_eq!(written, item.target);
        assert_eq!(fs::metadata(&output).unwrap().len(), item.target);
        assert!(image::open(&output).is_ok(), "PNG остаётся рабочим");
        let (parsed, copies, _carrier) = payload::extract_payload(&output, None).unwrap();
        assert_eq!(parsed.name, "secret.txt");
        assert_eq!(parsed.payload, b"hidden payload\n");
        assert!(copies >= payload::MIN_COPIES);
    }

    #[test]
    fn generate_one_rejects_a_target_smaller_than_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        fs::write(&secret, vec![7_u8; 32 * 1024]).unwrap();
        let item = GenerateItem {
            output: dir.path().join("made.png"),
            target: 16 * 1024,
            kind: GeneratedKind::Png,
            payloads: vec![secret],
        };
        let error = generate_one(
            &item,
            &ContainerOptions::default(),
            payload::MIN_COPIES,
            &Job::quiet(),
        )
        .unwrap_err();
        assert!(error.contains("минимум"), "{error}");
        assert!(!item.output.exists());
    }

    #[test]
    fn generated_names_clean_up_the_base() {
        assert_eq!(
            generated_name("  ", MIB, GeneratedKind::Gif, NameMode::Size),
            "file_1MB.gif"
        );
        assert_eq!(
            generated_name("photo.", MIB, GeneratedKind::Jpeg, NameMode::Plain),
            "photo.jpg"
        );
        let random = generated_name("photo", MIB, GeneratedKind::Webp, NameMode::Random);
        assert!(random.ends_with(".webp"), "{random}");
        assert!(!random.starts_with("photo"), "{random}");
    }
}
