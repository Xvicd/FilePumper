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

/// Сторона служебной картинки: она даёт валидный заголовок формата, а объём
/// добирается случайными данными внутри структуры файла.
const BASE_SIDE: u32 = 16;

/// Байты минимального валидного изображения выбранного типа.
///
/// Для обычного файла это один случайный байт: так у любого типа есть
/// «нулевой» носитель, к которому можно прицепить второе дно.
pub fn base_bytes(kind: GeneratedKind) -> Result<Vec<u8>, String> {
    if kind == GeneratedKind::Bin {
        return random_bytes(1);
    }
    let format = match kind {
        GeneratedKind::Png => image::ImageFormat::Png,
        GeneratedKind::Gif => image::ImageFormat::Gif,
        GeneratedKind::Jpeg => image::ImageFormat::Jpeg,
        GeneratedKind::Webp => image::ImageFormat::WebP,
        GeneratedKind::Bin => unreachable!("обычный файл обработан выше"),
    };
    // Шумовая картинка: она и сама плохо сжимается, и не выглядит заглушкой.
    let noise = random_bytes((BASE_SIDE * BASE_SIDE * 3) as usize)?;
    let image = image::RgbImage::from_raw(BASE_SIDE, BASE_SIDE, noise)
        .ok_or_else(|| "Не удалось собрать служебное изображение.".to_owned())?;
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), format)
        .map_err(|err| format!("Не удалось собрать служебное изображение: {err}"))?;
    Ok(bytes)
}

/// Минимально возможный файл этого типа: меньше служебной картинки не выйдет.
pub fn min_size(kind: GeneratedKind) -> Result<u64, String> {
    Ok(base_bytes(kind)?.len() as u64)
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
pub fn run(kind: GeneratedKind, output: &Path, target: u64, job: &Job<'_>) -> Result<u64, String> {
    job.check()?;
    if target == 0 {
        return Err("Размер файла должен быть больше нуля.".to_owned());
    }
    let base = base_bytes(kind)?;
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
            let len = run(kind, &output, target, &Job::quiet()).unwrap();
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
            run(kind, &output, target, &Job::quiet()).unwrap();
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
            run(*kind, &output, target, &Job::quiet()).unwrap();
            let picture = image::open(&output);
            assert!(picture.is_ok(), "тип {} не открылся", kind.title());
        }
    }

    #[test]
    fn two_runs_produce_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one.bin");
        let second = dir.path().join("two.bin");
        run(GeneratedKind::Bin, &first, 8192, &Job::quiet()).unwrap();
        run(GeneratedKind::Bin, &second, 8192, &Job::quiet()).unwrap();
        assert_ne!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    }

    #[test]
    fn too_small_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("tiny.png");
        let error = run(GeneratedKind::Png, &output, 16, &Job::quiet()).unwrap_err();
        assert!(error.contains("минимум"), "{error}");
        assert!(!output.exists());
        let zero = run(GeneratedKind::Bin, &output, 0, &Job::quiet()).unwrap_err();
        assert!(zero.contains("больше нуля"), "{zero}");
    }
}
