//! Пакетная накачка: несколько файлов за один запуск.
//!
//! Сами методы накачки работают с одной парой «вход → выход», поэтому пакетность
//! живёт здесь: пути результатов, последовательный прогон, общий прогресс и
//! отчёт, в котором сбой одного файла не отменяет остальные.

use std::fs;
use std::path::{Path, PathBuf};

use crate::pumper::{self, AnimationFormat, Method};
use crate::util::{same_file, unique_path, Job, MIB};

/// Один файл пакета: исходник и путь результата.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub input: PathBuf,
    pub output: PathBuf,
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
        })
    }
}

/// Результат по одному файлу пакета.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemResult {
    Created { output: PathBuf, size: u64 },
    Failed { message: String },
    Skipped,
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
    pumper::run(method, format, &item.input, &item.output, target_bytes, job)?;
    fs::metadata(&item.output)
        .map(|metadata| metadata.len())
        .map_err(|err| err.to_string())
}

/// Последовательно обрабатывает весь пакет.
///
/// Ошибка отдельного файла не прерывает пакет: она попадает в отчёт, обработка
/// продолжается. Отмена прерывает пакет, недоделанные файлы помечаются пропущенными.
pub fn pump(
    items: &[BatchItem],
    method: Method,
    format: AnimationFormat,
    target_bytes: u64,
    job: &Job<'_>,
) -> BatchReport {
    let total = items.len().max(1) as f32;
    let mut report = BatchReport::default();

    for (index, item) in items.iter().enumerate() {
        if report.cancelled || job.check().is_err() {
            report.cancelled = true;
            report.items.push((item.input.clone(), ItemResult::Skipped));
            continue;
        }
        let progress = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            job.report(overall, text);
        };
        let item_job = job.child(&progress);
        let result = match pump_one(item, method, format, target_bytes, &item_job) {
            Ok(size) => ItemResult::Created {
                output: item.output.clone(),
                size,
            },
            Err(message) if message == crate::util::CANCELLED_MESSAGE => {
                report.cancelled = true;
                ItemResult::Skipped
            }
            Err(message) => ItemResult::Failed { message },
        };
        report.items.push((item.input.clone(), result));
    }

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
        let report = pump(&items, Method::Padding, AnimationFormat::Gif, MIB, &job);
        assert_eq!(report.created(), 2);
        assert_eq!(report.failed(), 0);
        assert!(report.is_ok());
        for path in report.output_paths() {
            assert_eq!(fs::metadata(path).unwrap().len(), MIB);
        }

        let broken = vec![
            BatchItem {
                input: first,
                output: out_dir.join("a.bin"),
            },
            BatchItem {
                input: dir.path().join("missing.txt"),
                output: out_dir.join("b.bin"),
            },
        ];
        let report = pump(&broken, Method::Padding, AnimationFormat::Gif, MIB, &job);
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
        let report = pump(&items, Method::PngChunks, AnimationFormat::Gif, MIB, &job);

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
            BatchItem {
                input: input.clone(),
                output: dir.path().join("one.bin"),
            },
            BatchItem {
                input,
                output: dir.path().join("two.bin"),
            },
        ];
        let token = crate::util::CancelToken::new();
        token.cancel();
        let progress = |_: f32, _: &str| {};
        let job = Job::new(&progress, &token);
        let report = pump(&items, Method::Padding, AnimationFormat::Gif, MIB, &job);
        assert!(report.cancelled);
        assert_eq!(report.created(), 0);
        assert!(matches!(report.items[0].1, ItemResult::Skipped));
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
}
