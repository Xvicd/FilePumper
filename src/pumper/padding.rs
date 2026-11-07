//! Добивка файлов до нужного размера: приватные PNG-чанки, комментарий GIF,
//! случайные данные в конце.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::util::{random_bytes, write_random_stream, Job};

/// Максимальный размер одного чанка PNG.
const PNG_CHUNK_LIMIT: usize = 4 * 1024 * 1024;
const GIF_SUB_BLOCK: usize = 255;

pub fn run(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    fs::copy(input, output).map_err(|err| err.to_string())?;
    append_random_padding(output, target, job)
}

pub fn append_random_padding(output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if current >= target {
        return Ok(());
    }
    let needed = target - current;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(output)
        .map_err(|err| err.to_string())?;
    write_random_stream(&mut file, needed, job, "Добавление случайных данных")
}

fn temp_in_same_dir(path: &Path) -> Result<tempfile::NamedTempFile, String> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tempfile::NamedTempFile::new_in(parent).map_err(|err| err.to_string())
}

fn persist(temp: tempfile::NamedTempFile, path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|err| format!("Не удалось заменить файл: {err}"))?;
    }
    temp.persist(path)
        .map(|_| ())
        .map_err(|err| format!("Не удалось заменить файл: {}", err.error))
}

/// Добавляет приватные чанки `caBX` перед IEND — PNG остаётся полностью валидным.
pub fn append_png_padding(path: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(path).map_err(|err| err.to_string())?.len();
    if current >= target {
        return Ok(());
    }
    if current < 12 {
        return Err("Некорректный PNG: отсутствует завершающий IEND".into());
    }
    let mut source = File::open(path).map_err(|err| err.to_string())?;
    source
        .seek(SeekFrom::End(-12))
        .map_err(|err| err.to_string())?;
    let mut iend = [0_u8; 12];
    source
        .read_exact(&mut iend)
        .map_err(|err| err.to_string())?;
    if &iend[4..8] != b"IEND" {
        return Err("Некорректный PNG: завершающий IEND не найден".into());
    }
    source
        .seek(SeekFrom::Start(0))
        .map_err(|err| err.to_string())?;

    let mut temp = temp_in_same_dir(path)?;
    let mut prefix = source.take(current - 12);
    io::copy(&mut prefix, temp.as_file_mut()).map_err(|err| err.to_string())?;
    drop(prefix);

    let bytes_needed = target - current;
    let mut written = 0_u64;
    while written < bytes_needed {
        job.check()?;
        let payload_size = (bytes_needed - written).min(PNG_CHUNK_LIMIT as u64) as usize;
        let payload = random_bytes(payload_size)?;
        let chunk_type = b"caBX";
        let chunk_len =
            u32::try_from(payload_size).map_err(|_| "Слишком большой PNG-чанк".to_owned())?;
        temp.write_all(&chunk_len.to_be_bytes())
            .and_then(|_| temp.write_all(chunk_type))
            .and_then(|_| temp.write_all(&payload))
            .map_err(|err| err.to_string())?;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(chunk_type);
        hasher.update(&payload);
        temp.write_all(&hasher.finalize().to_be_bytes())
            .map_err(|err| err.to_string())?;
        written += payload_size as u64;
        job.report(
            written as f32 / bytes_needed as f32 * 100.0,
            "Добавление приватных PNG-чанков...",
        );
    }
    temp.write_all(&iend).map_err(|err| err.to_string())?;
    persist(temp, path)
}

/// Добавляет комментарий GIF перед трейлером: в отличие от мусора после
/// трейлера, такой файл остаётся валидным GIF для любых серверов.
pub fn append_gif_comment_padding(path: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(path).map_err(|err| err.to_string())?.len();
    if current >= target {
        return Ok(());
    }
    if current < 14 {
        return Err("Некорректный GIF: файл слишком мал.".into());
    }
    let mut source = File::open(path).map_err(|err| err.to_string())?;
    source
        .seek(SeekFrom::End(-1))
        .map_err(|err| err.to_string())?;
    let mut trailer = [0_u8; 1];
    source
        .read_exact(&mut trailer)
        .map_err(|err| err.to_string())?;
    if trailer[0] != 0x3B {
        return Err("Некорректный GIF: не найден трейлер.".into());
    }
    source
        .seek(SeekFrom::Start(0))
        .map_err(|err| err.to_string())?;

    let mut temp = temp_in_same_dir(path)?;
    let mut prefix = source.take(current - 1);
    io::copy(&mut prefix, temp.as_file_mut()).map_err(|err| err.to_string())?;
    drop(prefix);

    // 2 байта заголовка расширения + подблоки + терминатор блока.
    let needed = target - current;
    let mut data_len = needed.saturating_sub(3);
    loop {
        let blocks = data_len.div_ceil(GIF_SUB_BLOCK as u64);
        let next = needed.saturating_sub(3 + blocks);
        if next == data_len {
            break;
        }
        data_len = next;
    }
    temp.write_all(&[0x21, 0xFE])
        .map_err(|err| err.to_string())?;
    let mut remaining = data_len;
    while remaining > 0 {
        job.check()?;
        let size = remaining.min(GIF_SUB_BLOCK as u64) as usize;
        let bytes = random_bytes(size)?;
        temp.write_all(&[u8::try_from(size).unwrap_or(u8::MAX)])
            .and_then(|_| temp.write_all(&bytes))
            .map_err(|err| err.to_string())?;
        remaining -= size as u64;
        job.report(
            (data_len - remaining) as f32 / data_len.max(1) as f32 * 100.0,
            "Добавление комментария GIF...",
        );
    }
    temp.write_all(&[0x00, 0x3B])
        .map_err(|err| err.to_string())?;
    persist(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_reaches_target_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.bin");
        fs::write(&path, b"source").unwrap();
        let target = 4096;
        let output = path.with_file_name("output.bin");
        run(&path, &output, target, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(output).unwrap().len(), target);
    }

    #[test]
    fn png_padding_keeps_png_decodable_and_crc_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([20, 30, 40]));
        image.save(&path).unwrap();
        let original_len = fs::metadata(&path).unwrap().len();
        append_png_padding(&path, original_len + 17, &Job::quiet()).unwrap();

        let bytes = fs::read(&path).unwrap();
        assert!(image::open(&path).is_ok());
        let chunk_start = (original_len - 12) as usize;
        assert_eq!(&bytes[chunk_start + 4..chunk_start + 8], b"caBX");
        let payload = &bytes[chunk_start + 8..chunk_start + 8 + 17];
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(b"caBX");
        hasher.update(payload);
        let expected = hasher.finalize().to_be_bytes();
        assert_eq!(
            &bytes[chunk_start + 8 + 17..chunk_start + 12 + 17],
            expected.as_slice()
        );
    }

    #[test]
    fn gif_padding_keeps_gif_decodable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.gif");
        let image = image::RgbImage::from_pixel(3, 3, image::Rgb([1, 2, 3]));
        image.save(&path).unwrap();
        let start = fs::metadata(&path).unwrap().len();
        append_gif_comment_padding(&path, start + 1000, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), start + 1000);
        assert!(image::open(&path).is_ok());
    }

    #[test]
    fn padding_beyond_limit_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.bin");
        fs::write(&path, b"0123456789").unwrap();
        append_random_padding(&path, 5, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 10);
    }
}
