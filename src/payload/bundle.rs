//! Бандл: несколько файлов нагрузки в одном контейнере.
//!
//! Внутри контейнера лежит обычный ZIP без сжатия: данные и так едут внутри
//! носителя, а лишний deflate только замедляет работу. Имена очищаются так же,
//! как при обычном извлечении, поэтому путь из архива не может выйти за пределы
//! выбранной папки.

use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions as FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::util::{sanitize_file_name, unique_path, Job};

pub const BUNDLE_NAME: &str = "bundle.zip";

/// Один файл внутри бандла.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleEntry {
    pub name: String,
    pub size: u64,
}

/// Уникальное имя записи: `photo.png`, `photo-2.png`, …
fn unique_name(path: &Path, used: &mut HashSet<String>) -> String {
    let base = sanitize_file_name(
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("payload.bin"),
    );
    if used.insert(base.clone()) {
        return base;
    }
    let (stem, extension) = match base.rsplit_once('.') {
        Some((stem, extension)) => (stem.to_owned(), Some(extension.to_owned())),
        None => (base, None),
    };
    for index in 2..10_000_u32 {
        let candidate = match extension.as_deref() {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    format!("{stem}-bundle")
}

/// Собирает ZIP из файлов; общий прогресс считается по количеству файлов.
pub fn build(paths: &[PathBuf], job: &Job<'_>) -> Result<Vec<u8>, String> {
    if paths.is_empty() {
        return Err("Не указан ни один файл нагрузки.".to_owned());
    }
    let mut buffer = Cursor::new(Vec::new());
    let mut used = HashSet::new();
    {
        let mut writer = ZipWriter::new(&mut buffer);
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        for (index, path) in paths.iter().enumerate() {
            job.check()?;
            let data = fs::read(path)
                .map_err(|err| format!("Не удалось прочитать {}: {err}", path.display()))?;
            let name = unique_name(path, &mut used);
            writer
                .start_file(&name, options)
                .map_err(|err| format!("Не удалось добавить {name} в бандл: {err}"))?;
            writer.write_all(&data).map_err(|err| err.to_string())?;
            job.report(
                (index + 1) as f32 / paths.len() as f32 * 12.0,
                &format!("Бандл: {name} ({} байт)", data.len()),
            );
        }
        writer
            .finish()
            .map_err(|err| format!("Не удалось собрать бандл: {err}"))?;
    }
    Ok(buffer.into_inner())
}

/// Список файлов внутри бандла — без записи на диск.
pub fn list(bytes: &[u8]) -> Result<Vec<BundleEntry>, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|err| format!("Бандл повреждён: {err}"))?;
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|err| format!("Бандл повреждён: {err}"))?;
        entries.push(BundleEntry {
            name: sanitize_file_name(file.name()),
            size: file.size(),
        });
    }
    Ok(entries)
}

/// Распаковывает бандл в папку, не перезаписывая существующие файлы.
pub fn unpack(bytes: &[u8], out_dir: &Path, job: &Job<'_>) -> Result<Vec<(PathBuf, u64)>, String> {
    fs::create_dir_all(out_dir)
        .map_err(|err| format!("Не удалось создать папку {}: {err}", out_dir.display()))?;
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|err| format!("Бандл повреждён: {err}"))?;
    let total = archive.len().max(1);
    let mut written = Vec::new();
    for index in 0..archive.len() {
        job.check()?;
        let mut file = archive
            .by_index(index)
            .map_err(|err| format!("Бандл повреждён: {err}"))?;
        let name = sanitize_file_name(file.name());
        let destination = unique_path(&out_dir.join(&name));
        let mut data = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut data)
            .map_err(|err| format!("Не удалось прочитать {name}: {err}"))?;
        fs::write(&destination, &data)
            .map_err(|err| format!("Не удалось записать {}: {err}", destination.display()))?;
        job.report(
            (index + 1) as f32 / total as f32 * 100.0,
            &format!("Распаковка: {name}"),
        );
        written.push((destination, data.len() as u64));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_files(dir: &Path) -> Vec<PathBuf> {
        let first = dir.join("note.txt");
        let second = dir.join("photo.png");
        fs::write(&first, b"hello bundle").unwrap();
        fs::write(&second, b"second payload").unwrap();
        vec![first, second]
    }

    #[test]
    fn bundle_roundtrip_keeps_names_and_data() {
        let dir = tempfile::tempdir().unwrap();
        let files = sample_files(dir.path());
        let bytes = build(&files, &Job::quiet()).unwrap();
        let entries = list(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "note.txt");
        assert_eq!(entries[1].name, "photo.png");

        let out_dir = dir.path().join("restored");
        let written = unpack(&bytes, &out_dir, &Job::quiet()).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(fs::read(out_dir.join("note.txt")).unwrap(), b"hello bundle");
        assert_eq!(
            fs::read(out_dir.join("photo.png")).unwrap(),
            b"second payload"
        );
    }

    #[test]
    fn duplicate_names_get_suffixes_and_unpack_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first_dir = dir.path().join("a");
        let second_dir = dir.path().join("b");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        fs::write(first_dir.join("same.txt"), b"one").unwrap();
        fs::write(second_dir.join("same.txt"), b"two").unwrap();
        let bytes = build(
            &[first_dir.join("same.txt"), second_dir.join("same.txt")],
            &Job::quiet(),
        )
        .unwrap();
        let names: Vec<String> = list(&bytes).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["same.txt", "same-2.txt"]);

        let out_dir = dir.path().join("out");
        fs::create_dir_all(&out_dir).unwrap();
        fs::write(out_dir.join("same.txt"), b"already here").unwrap();
        let written = unpack(&bytes, &out_dir, &Job::quiet()).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(
            fs::read(out_dir.join("same.txt")).unwrap(),
            b"already here",
            "существующий файл не должен перезаписываться"
        );
        // Имена не совпадают, поэтому второй файл получает свободное имя от
        // первого результата (`same-2-2.txt`) — важно лишь, что данные целы.
        let mut contents: Vec<Vec<u8>> = fs::read_dir(&out_dir)
            .unwrap()
            .map(|entry| fs::read(entry.unwrap().path()).unwrap())
            .collect();
        contents.sort();
        let mut expected = vec![b"already here".to_vec(), b"one".to_vec(), b"two".to_vec()];
        expected.sort();
        assert_eq!(contents, expected);
    }

    #[test]
    fn empty_bundle_is_rejected() {
        assert!(build(&[], &Job::quiet()).is_err());
    }

    #[test]
    fn broken_bundle_reports_error() {
        assert!(list(b"not a zip at all").is_err());
    }
}
