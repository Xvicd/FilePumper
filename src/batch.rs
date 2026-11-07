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

use crate::pumper::{self, AnimationFormat, Method};
use crate::util::{same_file, unique_path, CancelToken, Job, Reservations, MIB};

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
    ) -> Result<Self, String> {
        if !input.is_file() {
            return Err("файл не найден".to_owned());
        }
        let output = output_path(input, target_bytes, method, format, out_dir)?;
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

/// Готовый план пакета: настройки, размеры и список входов.
#[derive(Clone, Debug)]
pub struct PumpPlan {
    pub method: Method,
    pub format: AnimationFormat,
    pub out_dir: Option<PathBuf>,
    pub mode: TargetMode,
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
        Ok(Self {
            method,
            format,
            out_dir,
            mode,
            targets,
        })
    }

    /// Размер для файла номер `index` (если размеров меньше — повторяется последний).
    pub fn target_for(&self, index: usize, input_len: u64) -> u64 {
        let base = self.targets[index.min(self.targets.len() - 1)];
        self.mode.target_bytes(base, input_len)
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
        let name = result_name(input, target, self.method, self.format);
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

/// Имя результата: `photo.jpg` → `photo_10MB.png` (расширение зависит от метода).
pub fn result_name(
    input: &Path,
    target_bytes: u64,
    method: Method,
    format: AnimationFormat,
) -> String {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    format!(
        "{stem}_{}.{}",
        size_label(target_bytes),
        output_extension(method, format, input)
    )
}

/// Путь результата: рядом с исходником или в указанной папке, без перезаписи
/// существующих файлов.
pub fn output_path(
    input: &Path,
    target_bytes: u64,
    method: Method,
    format: AnimationFormat,
    out_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let name = result_name(input, target_bytes, method, format);
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
            result_name(input, 5 * MIB, Method::PngChunks, AnimationFormat::Gif),
            "IMG_0001_5MB.png"
        );
        assert_eq!(
            result_name(input, 5 * MIB, Method::Animation, AnimationFormat::Webp),
            "IMG_0001_5MB.webp"
        );
        assert_eq!(
            result_name(input, 5 * MIB, Method::Padding, AnimationFormat::Gif),
            "IMG_0001_5MB.jpeg"
        );
        assert_eq!(
            result_name(input, 5 * MIB, Method::Svg, AnimationFormat::Gif),
            "IMG_0001_5MB.svg"
        );
    }

    #[test]
    fn output_path_never_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.png");
        fs::write(&input, b"x").unwrap();
        let first = output_path(&input, MIB, Method::Padding, AnimationFormat::Gif, None).unwrap();
        assert_eq!(first, dir.path().join("photo_1MB.png"));
        fs::write(&first, b"y").unwrap();
        let second = output_path(&input, MIB, Method::Padding, AnimationFormat::Gif, None).unwrap();
        assert_eq!(second, dir.path().join("photo_1MB-2.png"));
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
            )
            .unwrap(),
            BatchItem::new(
                &second,
                MIB,
                Method::Padding,
                AnimationFormat::Gif,
                Some(&out_dir),
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
            )
            .unwrap(),
            BatchItem::new(
                &png,
                MIB,
                Method::PngChunks,
                AnimationFormat::Gif,
                Some(&out_dir),
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
}
