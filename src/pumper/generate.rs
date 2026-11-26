//! Создание файлов «с нуля»: нужный размер без исходника.
//!
//! Внутри всегда криптослучайные байты (`getrandom`), поэтому архив не
//! уменьшит результат: `zip`/`gzip`/`7z` вернут примерно тот же объём.
//! Картиночные типы начинаются с маленького валидного изображения, а объём
//! добирается так, чтобы файл оставался рабочим: приватные чанки PNG, блок
//! комментария GIF, хвост случайных данных для JPEG и WebP.

use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::Path;

use crate::pumper::{padding, GeneratedKind};
use crate::util::{random_bytes, Job};

/// Сторона служебной картинки по умолчанию: она даёт валидный заголовок формата,
/// а объём добирается случайными данными внутри структуры файла.
const BASE_SIDE: u32 = 16;

/// Размер видимой «шумовой» картинки внутри созданного файла.
///
/// Объём файла задаётся отдельно (`target`), а картинка решает, как файл
/// выглядит: 512×512 — это уже заметная шумовая фотография, а не заглушка
/// 16×16 с хвостом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

impl Default for ImageSize {
    fn default() -> Self {
        Self {
            width: BASE_SIDE,
            height: BASE_SIDE,
        }
    }
}

impl ImageSize {
    /// Меньше — картинка вырождается в точку.
    pub const MIN_SIDE: u32 = 8;
    /// Больше — слишком много памяти и времени на кодирование.
    pub const MAX_SIDE: u32 = 4096;
    /// Границы для подсказок в интерфейсе и справке.
    pub const SIDE_RANGE: std::ops::RangeInclusive<u32> = Self::MIN_SIDE..=Self::MAX_SIDE;

    /// Приводит стороны в допустимые границы.
    pub fn sanitized(self) -> Self {
        Self {
            width: self.width.clamp(Self::MIN_SIDE, Self::MAX_SIDE),
            height: self.height.clamp(Self::MIN_SIDE, Self::MAX_SIDE),
        }
    }

    /// Разбирает `512x512`, `512X512`, `512×512`, `512*512`, `512,512` и `512`
    /// (квадрат). Размеры вне [`ImageSize::SIDE_RANGE`] — ошибка.
    pub fn parse(value: &str) -> Result<Self, String> {
        let normalized = value
            .trim()
            .to_ascii_lowercase()
            .replace(['х', '×', '*', ' ', ','], "x");
        let parts: Vec<&str> = normalized
            .split('x')
            .filter(|part| !part.is_empty())
            .collect();
        let (width, height) = match parts.as_slice() {
            [side] => (*side, *side),
            [width, height] => (*width, *height),
            _ => {
                return Err(format!(
                    "Размер картинки: укажите «512x512» или одно число, например 512 (получено «{value}»)."
                ))
            }
        };
        let parse_side = |raw: &str| -> Result<u32, String> {
            raw.parse::<u32>()
                .map_err(|_| format!("Неверный размер картинки: «{raw}». Нужно число пикселей."))
        };
        let size = Self {
            width: parse_side(width)?,
            height: parse_side(height)?,
        };
        if !Self::SIDE_RANGE.contains(&size.width) || !Self::SIDE_RANGE.contains(&size.height) {
            return Err(format!(
                "Сторона картинки — от {} до {} пикселей.",
                Self::MIN_SIDE,
                Self::MAX_SIDE
            ));
        }
        Ok(size)
    }

    /// Подпись для интерфейса и отчёта: `512×512`.
    pub fn label(self) -> String {
        format!("{}×{}", self.width, self.height)
    }
}

/// Байты минимального валидного изображения выбранного типа.
///
/// Для обычного файла это один случайный байт: так у любого типа есть
/// «нулевой» носитель, к которому можно прицепить второе дно.
pub fn base_bytes(kind: GeneratedKind, size: ImageSize) -> Result<Vec<u8>, String> {
    if kind == GeneratedKind::Bin {
        return random_bytes(1);
    }
    let size = size.sanitized();
    let format = match kind {
        GeneratedKind::Png => image::ImageFormat::Png,
        GeneratedKind::Gif => image::ImageFormat::Gif,
        GeneratedKind::Jpeg => image::ImageFormat::Jpeg,
        GeneratedKind::Webp => image::ImageFormat::WebP,
        GeneratedKind::Bin => unreachable!("обычный файл обработан выше"),
    };
    // Шумовая картинка: она и сама плохо сжимается, и не выглядит заглушкой.
    let pixels = u64::from(size.width) * u64::from(size.height) * 3;
    let noise =
        random_bytes(usize::try_from(pixels).map_err(|_| "Слишком большая картинка.".to_owned())?)?;
    let image = image::RgbImage::from_raw(size.width, size.height, noise)
        .ok_or_else(|| "Не удалось собрать служебное изображение.".to_owned())?;
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), format)
        .map_err(|err| format!("Не удалось собрать служебное изображение: {err}"))?;
    Ok(bytes)
}

/// Минимально возможный файл этого типа: меньше служебной картинки не выйдет.
pub fn min_size(kind: GeneratedKind, size: ImageSize) -> Result<u64, String> {
    Ok(base_bytes(kind, size)?.len() as u64)
}

/// Дописывает файл до `target` так, чтобы он остался рабочим.
pub fn pad_to(kind: GeneratedKind, path: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    match kind {
        GeneratedKind::Png => padding::append_png_padding(path, target, job),
        GeneratedKind::Gif => padding::append_gif_comment_padding(path, target, job),
        GeneratedKind::Bin | GeneratedKind::Jpeg | GeneratedKind::Webp => {
            padding::append_random_padding(path, target, job)
        }
    }
}

/// Создаёт файл ровно указанного размера и возвращает его длину.
pub fn run(
    kind: GeneratedKind,
    output: &Path,
    target: u64,
    image: ImageSize,
    job: &Job<'_>,
) -> Result<u64, String> {
    job.check()?;
    if target == 0 {
        return Err("Размер файла должен быть больше нуля.".to_owned());
    }
    let base = base_bytes(kind, image)?;
    if base.len() as u64 > target {
        return Err(format!(
            "Для файла типа «{}» нужно минимум {} байт.",
            kind.title(),
            base.len()
        ));
    }
    job.report(2.0, "Генерация случайных данных...");
    let mut file = File::create(output).map_err(|err| err.to_string())?;
    file.write_all(&base).map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())?;
    drop(file);
    pad_to(kind, output, target, job)?;
    let len = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if len != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {len}."
        ));
    }
    job.report(100.0, "Готово.");
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deflate_len(bytes: &[u8]) -> u64 {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap().len() as u64
    }

    #[test]
    fn every_kind_reaches_exact_size() {
        let dir = tempfile::tempdir().unwrap();
        let target = 256 * 1024;
        for kind in GeneratedKind::ALL {
            let output = dir.path().join(format!("made.{}", kind.extension()));
            let len = run(kind, &output, target, ImageSize::default(), &Job::quiet()).unwrap();
            assert_eq!(len, target, "тип {}", kind.title());
            assert_eq!(fs::metadata(&output).unwrap().len(), target);
        }
    }

    #[test]
    fn generated_content_cannot_be_compressed() {
        let dir = tempfile::tempdir().unwrap();
        let target = 512 * 1024;
        for kind in GeneratedKind::ALL {
            let output = dir.path().join(format!("made.{}", kind.extension()));
            run(kind, &output, target, ImageSize::default(), &Job::quiet()).unwrap();
            let bytes = fs::read(&output).unwrap();
            let packed = deflate_len(&bytes);
            assert!(
                packed as f64 > target as f64 * 0.95,
                "тип {} сжался до {packed} из {target}",
                kind.title()
            );
        }
    }

    #[test]
    fn image_kinds_stay_decodable_after_padding() {
        let dir = tempfile::tempdir().unwrap();
        let target = 128 * 1024;
        for kind in GeneratedKind::ALL.iter().filter(|kind| kind.is_image()) {
            let output = dir.path().join(format!("made.{}", kind.extension()));
            run(*kind, &output, target, ImageSize::default(), &Job::quiet()).unwrap();
            let picture = image::open(&output);
            assert!(picture.is_ok(), "тип {} не открылся", kind.title());
        }
    }

    #[test]
    fn image_size_parses_square_and_rectangle() {
        assert_eq!(
            ImageSize::parse("512").unwrap(),
            ImageSize {
                width: 512,
                height: 512
            }
        );
        assert_eq!(
            ImageSize::parse("640x480").unwrap(),
            ImageSize {
                width: 640,
                height: 480
            }
        );
        assert_eq!(
            ImageSize::parse("640×480").unwrap(),
            ImageSize {
                width: 640,
                height: 480
            }
        );
        assert!(ImageSize::parse("0x100").is_err());
        assert!(ImageSize::parse("10000x100").is_err());
        assert!(ImageSize::parse("abc").is_err());
        assert_eq!(ImageSize::default().label(), "16×16");
    }

    #[test]
    fn bigger_picture_is_the_visible_part_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("noise.png");
        let target = 4 * 1024 * 1024;
        run(
            GeneratedKind::Png,
            &output,
            target,
            ImageSize {
                width: 512,
                height: 512,
            },
            &Job::quiet(),
        )
        .unwrap();
        let picture = image::open(&output).unwrap();
        assert_eq!((picture.width(), picture.height()), (512, 512));
        assert_eq!(fs::metadata(&output).unwrap().len(), target);
    }

    #[test]
    fn two_runs_produce_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one.bin");
        let second = dir.path().join("two.bin");
        run(
            GeneratedKind::Bin,
            &first,
            8192,
            ImageSize::default(),
            &Job::quiet(),
        )
        .unwrap();
        run(
            GeneratedKind::Bin,
            &second,
            8192,
            ImageSize::default(),
            &Job::quiet(),
        )
        .unwrap();
        assert_ne!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    }

    #[test]
    fn too_small_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("tiny.png");
        let error = run(
            GeneratedKind::Png,
            &output,
            16,
            ImageSize::default(),
            &Job::quiet(),
        )
        .unwrap_err();
        assert!(error.contains("минимум"), "{error}");
        assert!(!output.exists());
        let zero = run(
            GeneratedKind::Bin,
            &output,
            0,
            ImageSize::default(),
            &Job::quiet(),
        )
        .unwrap_err();
        assert!(zero.contains("больше нуля"), "{zero}");
    }
}
