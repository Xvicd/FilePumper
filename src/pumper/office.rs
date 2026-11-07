//! Режим 5: ZIP/Office (несжатая запись), PDF (поток-заполнитель),
//! прочие файлы — обычное добавление данных в конец.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use zip::write::SimpleFileOptions as FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::pumper::padding::append_random_padding;
use crate::util::{write_random_stream, Job};

pub fn run(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    let extension = input
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let current = fs::metadata(input).map_err(|err| err.to_string())?.len();
    if current >= target {
        fs::copy(input, output).map_err(|err| err.to_string())?;
        return Ok(());
    }
    let needed = target - current;

    if let Ok(archive_file) = File::open(input) {
        if ZipArchive::new(archive_file).is_ok() {
            job.report(10.0, "Добавление несжатой записи в ZIP/Office-контейнер...");
            fs::copy(input, output).map_err(|err| err.to_string())?;
            let output_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(output)
                .map_err(|err| err.to_string())?;
            let mut writer = ZipWriter::new_append(output_file)
                .map_err(|err| format!("Не удалось открыть ZIP-контейнер: {err}"))?;
            let options = FileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .large_file(true);
            writer
                .start_file("word_media_cache.bin", options)
                .map_err(|err| format!("Не удалось добавить ZIP-запись: {err}"))?;
            write_random_stream(&mut writer, needed, job, "Добавлено данных")?;
            writer
                .finish()
                .map_err(|err| format!("Не удалось завершить ZIP: {err}"))?;
            return Ok(());
        }
    }

    if extension == "pdf" {
        job.report(5.0, "Добавление потока-заполнителя в PDF...");
        let mut data = Vec::new();
        File::open(input)
            .and_then(|mut file| file.read_to_end(&mut data))
            .map_err(|err| err.to_string())?;
        let eof = data
            .windows(5)
            .rposition(|window| window == b"%%EOF")
            .unwrap_or(data.len());
        let mut file = File::create(output).map_err(|err| err.to_string())?;
        file.write_all(&data[..eof])
            .map_err(|err| err.to_string())?;
        write!(file, "\n99999 0 obj\n<< /Length {needed} >>\nstream\n")
            .map_err(|err| err.to_string())?;
        write_random_stream(&mut file, needed, job, "Запись PDF-потока")?;
        file.write_all(b"\nendstream\nendobj\n%%EOF\n")
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    append_random_padding_after_copy(input, output, target, job)
}

fn append_random_padding_after_copy(
    input: &Path,
    output: &Path,
    target: u64,
    job: &Job<'_>,
) -> Result<(), String> {
    job.report(5.0, "Копирование файла...");
    fs::copy(input, output).map_err(|err| err.to_string())?;
    append_random_padding(output, target, job)
}
