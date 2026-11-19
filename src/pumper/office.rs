//! Режим 5: ZIP/Office (несжатая запись), PDF (поток-заполнитель),
//! прочие файлы — обычное добавление данных в конец.
//!
//! Пакеты OPC (`.docx`, `.xlsx`, `.pptx`) Word, Excel и PowerPoint проверяют
//! строго: каждая часть архива обязана быть объявлена в `[Content_Types].xml`,
//! а записи не должны требовать ZIP64. Поэтому такие файлы не дописываются, а
//! пересобираются: служебный файл объявлений получает тип содержимого для
//! части-заполнителя, остальные части копируются без распаковки
//! ([`ZipWriter::raw_copy_file`]). Заполнитель добавляется отдельной несжатой
//! записью без ZIP64, поэтому пакет остаётся валидным для Office.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use zip::ZipArchive;
use zip::ZipWriter;

use crate::pumper::padding::append_random_padding;
use crate::util::{random_bytes, write_random_stream, Job, IO_CHUNK};

/// Служебная часть OPC со списком типов содержимого.
const CONTENT_TYPES_PART: &str = "[Content_Types].xml";
/// Общая часть имён заполнителя внутри пакета OPC.
const PADDING_PREFIX: &str = "filepumper/padding";
/// Часть-заполнитель внутри пакета OPC.
const PADDING_PART: &str = "filepumper/padding.fppad";
/// Расширение части-заполнителя.
const PADDING_EXTENSION: &str = "fppad";
/// Объявление расширения заполнителя в `[Content_Types].xml`.
const PADDING_DECLARATION: &str =
    "<Default Extension=\"fppad\" ContentType=\"application/octet-stream\"/>";
/// Записи заполнения, которые добавляли прежние версии: тип содержимого у них
/// не объявлялся, из-за чего Word считал документ повреждённым. При пересборке
/// такие записи выбрасываются, а нужный размер добивается заново.
const LEGACY_PADDING_PARTS: [&str; 1] = ["word_media_cache.bin"];

/// Граница, после которой ZIP требует 64-битные поля.
const ZIP64_LIMIT: u64 = u32::MAX as u64;
/// Максимальная длина комментария ZIP: столько байт можно добрать в конце
/// архива, не меняя ни одной записи.
const ZIP_COMMENT_LIMIT: u64 = 65_535;

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
        if let Ok(mut archive) = ZipArchive::new(archive_file) {
            let is_opc_package = archive.index_for_name(CONTENT_TYPES_PART).is_some();
            // Маленький зазор дешевле и безопаснее закрыть комментарием ZIP:
            // структура архива и пакета при этом не меняется вовсе.
            if needed <= ZIP_COMMENT_LIMIT && archive.comment().is_empty() {
                fs::copy(input, output).map_err(|err| err.to_string())?;
                if write_zip_comment(output, needed).is_ok() {
                    return Ok(());
                }
            }
            if is_opc_package {
                job.report(
                    5.0,
                    "Пересборка пакета Office с объявленной частью заполнения...",
                );
                rebuild_opc_package(&mut archive, output, job)?;
            } else {
                job.report(10.0, "Добавление несжатой записи в ZIP-контейнер...");
                fs::copy(input, output).map_err(|err| err.to_string())?;
            }
            return pad_zip_to_target(output, target, job);
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

/// Пересобирает пакет OPC: `[Content_Types].xml` первым и с объявлением
/// заполнителя, остальные части — побайтово, без распаковки.
fn rebuild_opc_package<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    output: &Path,
    job: &Job<'_>,
) -> Result<(), String> {
    let content_types_index = archive
        .index_for_name(CONTENT_TYPES_PART)
        .ok_or_else(|| format!("В пакете нет «{CONTENT_TYPES_PART}»."))?;
    let mut content_types = Vec::new();
    archive
        .by_name(CONTENT_TYPES_PART)
        .map_err(|err| format!("Не удалось прочитать «{CONTENT_TYPES_PART}»: {err}"))?
        .read_to_end(&mut content_types)
        .map_err(|err| err.to_string())?;
    let declared = declare_padding_content_type(&content_types)?;
    let rewrite_content_types = declared != content_types;

    let mut writer = ZipWriter::new(File::create(output).map_err(|err| err.to_string())?);

    // Типы содержимого обязаны идти первыми: от этого зависит разбор пакета.
    {
        let entry = archive
            .by_index_raw(content_types_index)
            .map_err(|err| err.to_string())?;
        if rewrite_content_types {
            let options = entry.options();
            drop(entry);
            writer
                .start_file(CONTENT_TYPES_PART, options)
                .map_err(|err| format!("Не удалось записать «{CONTENT_TYPES_PART}»: {err}"))?;
            writer.write_all(&declared).map_err(|err| err.to_string())?;
        } else {
            writer
                .raw_copy_file(entry)
                .map_err(|err| format!("Не удалось скопировать «{CONTENT_TYPES_PART}»: {err}"))?;
        }
    }

    for index in 0..archive.len() {
        job.check()?;
        if index == content_types_index {
            continue;
        }
        let name = {
            let entry = archive.by_index_raw(index).map_err(|err| err.to_string())?;
            entry.name().to_owned()
        };
        if LEGACY_PADDING_PARTS.contains(&name.as_str()) || name.starts_with(PADDING_PREFIX) {
            job.report(50.0, "Замена прежней части заполнения...");
            continue;
        }
        let entry = archive.by_index_raw(index).map_err(|err| err.to_string())?;
        writer
            .raw_copy_file(entry)
            .map_err(|err| format!("Не удалось скопировать «{name}»: {err}"))?;
    }
    writer
        .finish()
        .map_err(|err| format!("Не удалось завершить ZIP: {err}"))?;
    Ok(())
}

/// Добавляет в `[Content_Types].xml` объявление расширения заполнителя.
///
/// Без этого Word видит часть, у которой нет типа содержимого, и предлагает
/// «восстановить» документ — с потерей этой части.
fn declare_padding_content_type(content_types: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(content_types)
        .map_err(|_| format!("Некорректный пакет Office: «{CONTENT_TYPES_PART}» не в UTF-8."))?;
    let extension = format!("Extension=\"{PADDING_EXTENSION}\"");
    if text.contains(&extension) {
        return Ok(content_types.to_vec());
    }
    let close = text.rfind("</Types").ok_or_else(|| {
        format!("Некорректный пакет Office: в «{CONTENT_TYPES_PART}» нет тега </Types>.")
    })?;
    if !text[close..].contains('>') {
        return Err(format!(
            "Некорректный пакет Office: в «{CONTENT_TYPES_PART}» закрывающий тег </Types> обрывается."
        ));
    }
    let mut result = Vec::with_capacity(content_types.len() + PADDING_DECLARATION.len());
    result.extend_from_slice(&content_types[..close]);
    result.extend_from_slice(PADDING_DECLARATION.as_bytes());
    result.extend_from_slice(&content_types[close..]);
    Ok(result)
}

/// Размеры хвоста ZIP: где начинается центральный каталог, сколько он
/// занимает и какой комментарий архива нужно сохранить.
struct ZipTail {
    central_offset: u64,
    central_size: u64,
    entries: u16,
    comment: Vec<u8>,
}

/// Добивает готовый архив до точного размера одной несжатой записью.
///
/// Запись вставляется перед центральным каталогом вручную: библиотека `zip`
/// при дописывании пересобирает каталог и добавляет служебные поля, из-за чего
/// точный размер перестаёт быть предсказуемым, а пакет Office — валидным.
/// Здесь каталог копируется как есть, поэтому в файл добавляются ровно
/// `30 + имя` (локальный заголовок), данные заполнителя и `46 + имя` (запись
/// каталога).
fn pad_zip_to_target(output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let base = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if base >= target {
        return Err(format!(
            "Контейнер уже занимает {base} байт, а цель — {target}: надо выбрать размер больше."
        ));
    }
    let tail = read_zip_tail(output, base)?;
    let name = free_padding_name(output, &tail)?;
    let fixed = tail.central_offset
        + (30 + name.len() as u64)
        + tail.central_size
        + (46 + name.len() as u64)
        + 22
        + tail.comment.len() as u64;
    if target < fixed {
        return Err(format!(
            "Нельзя попасть точно в {target} байт: минимальный размер после пересборки — {fixed}."
        ));
    }
    let data_len = target - fixed;
    if data_len >= ZIP64_LIMIT {
        return Err(format!(
            "Пакет такого размера ({target} байт) потребовал бы ZIP64, а Office его не читает."
        ));
    }
    append_stored_entry(output, &name, data_len, &tail, job)?;
    ensure_size(output, target)
}

fn ensure_size(path: &Path, target: u64) -> Result<(), String> {
    let size = fs::metadata(path).map_err(|err| err.to_string())?.len();
    if size == target {
        Ok(())
    } else {
        Err(format!(
            "Не удалось попасть точно в {target} байт: получилось {size}."
        ))
    }
}

/// Читает конец архива: смещение и размер центрального каталога, число записей.
fn read_zip_tail(path: &Path, size: u64) -> Result<ZipTail, String> {
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let tail_len = size.min(22 + ZIP_COMMENT_LIMIT) as usize;
    if tail_len < 22 {
        return Err("Не удалось разобрать архив: файл слишком короткий.".to_owned());
    }
    file.seek(SeekFrom::Start(size - tail_len as u64))
        .map_err(|err| err.to_string())?;
    let mut tail = vec![0_u8; tail_len];
    file.read_exact(&mut tail).map_err(|err| err.to_string())?;
    let end = (0..tail.len().saturating_sub(3))
        .rev()
        .find(|&index| &tail[index..index + 4] == b"PK\x05\x06")
        .ok_or_else(|| "Не удалось разобрать архив: конец каталога не найден.".to_owned())?;
    let comment_len = u16::from_le_bytes([tail[end + 20], tail[end + 21]]) as usize;
    if end + 22 + comment_len != tail.len() {
        return Err(
            "Не удалось разобрать архив: после конца каталога есть лишние данные.".to_owned(),
        );
    }
    let comment = tail[end + 22..].to_vec();
    let entries = u16::from_le_bytes([tail[end + 10], tail[end + 11]]);
    let central_size = u32::from_le_bytes([
        tail[end + 12],
        tail[end + 13],
        tail[end + 14],
        tail[end + 15],
    ]);
    let central_offset = u32::from_le_bytes([
        tail[end + 16],
        tail[end + 17],
        tail[end + 18],
        tail[end + 19],
    ]);
    if entries == u16::MAX || central_size == u32::MAX || central_offset == u32::MAX {
        return Err(
            "Не удалось разобрать архив: ZIP64-контейнеры этим режимом не поддерживаются."
                .to_owned(),
        );
    }
    Ok(ZipTail {
        central_offset: central_offset as u64,
        central_size: central_size as u64,
        entries,
        comment,
    })
}

/// Свободное имя части заполнителя: при повторной накачке имя занимается.
fn free_padding_name(path: &Path, tail: &ZipTail) -> Result<String, String> {
    let archive = ZipArchive::new(File::open(path).map_err(|err| err.to_string())?)
        .map_err(|err| format!("Не удалось открыть ZIP-контейнер: {err}"))?;
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    drop(archive);
    if !names.iter().any(|name| name == PADDING_PART) {
        return Ok(PADDING_PART.to_owned());
    }
    for index in 2..1000 {
        let candidate = format!("{PADDING_PREFIX}-{index}.fppad");
        if !names.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Ok(format!("{PADDING_PREFIX}-{}.fppad", tail.entries))
}

/// Вставляет в архив несжатую запись с `data_len` случайных байт.
fn append_stored_entry(
    path: &Path,
    name: &str,
    data_len: u64,
    tail: &ZipTail,
    job: &Job<'_>,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|err| err.to_string())?;

    // Каталог перезапишется заполнителем, поэтому запоминаем его в памяти.
    file.seek(SeekFrom::Start(tail.central_offset))
        .map_err(|err| err.to_string())?;
    let mut central_directory = vec![0_u8; tail.central_size as usize];
    file.read_exact(&mut central_directory)
        .map_err(|err| err.to_string())?;

    let name_bytes = name.as_bytes();
    let local_offset = tail.central_offset;
    let mut header = Vec::with_capacity(30 + name_bytes.len());
    header.extend_from_slice(b"PK\x03\x04");
    header.extend_from_slice(&20_u16.to_le_bytes()); // версия распаковки
    header.extend_from_slice(&0_u16.to_le_bytes()); // флаги
    header.extend_from_slice(&0_u16.to_le_bytes()); // без сжатия
    header.extend_from_slice(&0_u16.to_le_bytes()); // время 00:00
    header.extend_from_slice(&0x0021_u16.to_le_bytes()); // дата 1980-01-01
    header.extend_from_slice(&0_u32.to_le_bytes()); // CRC32 — ниже
    header.extend_from_slice(&0_u32.to_le_bytes()); // сжатый размер
    header.extend_from_slice(&0_u32.to_le_bytes()); // исходный размер
    header.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    header.extend_from_slice(&0_u16.to_le_bytes()); // доп. поле
    header.extend_from_slice(name_bytes);
    file.seek(SeekFrom::Start(local_offset))
        .map_err(|err| err.to_string())?;
    file.write_all(&header).map_err(|err| err.to_string())?;

    let mut hasher = crc32fast::Hasher::new();
    let mut written = 0_u64;
    while written < data_len {
        job.check()?;
        let size = (data_len - written).min(IO_CHUNK as u64) as usize;
        let bytes = random_bytes(size)?;
        hasher.update(&bytes);
        file.write_all(&bytes).map_err(|err| err.to_string())?;
        written += size as u64;
        job.report(
            written as f32 / data_len.max(1) as f32 * 100.0,
            "Добавление данных...",
        );
    }
    let crc = hasher.finalize();
    let size = u32::try_from(data_len).map_err(|_| "Заполнитель больше 4 ГБ.".to_owned())?;

    // Поля CRC и размеров в локальном заголовке правятся после записи данных.
    file.seek(SeekFrom::Start(local_offset + 14))
        .map_err(|err| err.to_string())?;
    file.write_all(&crc.to_le_bytes())
        .and_then(|_| file.write_all(&size.to_le_bytes()))
        .and_then(|_| file.write_all(&size.to_le_bytes()))
        .map_err(|err| err.to_string())?;

    let central_new_offset = local_offset + header.len() as u64 + data_len;
    file.seek(SeekFrom::Start(central_new_offset))
        .map_err(|err| err.to_string())?;
    file.write_all(&central_directory)
        .map_err(|err| err.to_string())?;

    let mut record = Vec::with_capacity(46 + name_bytes.len());
    record.extend_from_slice(b"PK\x01\x02");
    record.extend_from_slice(&20_u16.to_le_bytes()); // версия создателя
    record.extend_from_slice(&20_u16.to_le_bytes()); // версия распаковки
    record.extend_from_slice(&0_u16.to_le_bytes()); // флаги
    record.extend_from_slice(&0_u16.to_le_bytes()); // без сжатия
    record.extend_from_slice(&0_u16.to_le_bytes()); // время 00:00
    record.extend_from_slice(&0x0021_u16.to_le_bytes()); // дата 1980-01-01
    record.extend_from_slice(&crc.to_le_bytes());
    record.extend_from_slice(&size.to_le_bytes());
    record.extend_from_slice(&size.to_le_bytes());
    record.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    record.extend_from_slice(&0_u16.to_le_bytes()); // доп. поле
    record.extend_from_slice(&0_u16.to_le_bytes()); // комментарий
    record.extend_from_slice(&0_u16.to_le_bytes()); // номер диска
    record.extend_from_slice(&0_u16.to_le_bytes()); // внутренние атрибуты
    record.extend_from_slice(&0x20_u32.to_le_bytes()); // внешние атрибуты
    record.extend_from_slice(
        &u32::try_from(local_offset)
            .map_err(|_| "Смещение записи не влезает в ZIP.".to_owned())?
            .to_le_bytes(),
    );
    record.extend_from_slice(name_bytes);
    file.write_all(&record).map_err(|err| err.to_string())?;

    let entries = tail.entries + 1;
    let central_size = tail.central_size + record.len() as u64;
    let mut end = Vec::with_capacity(22);
    end.extend_from_slice(b"PK\x05\x06");
    end.extend_from_slice(&0_u16.to_le_bytes()); // номер диска
    end.extend_from_slice(&0_u16.to_le_bytes()); // диск каталога
    end.extend_from_slice(&entries.to_le_bytes());
    end.extend_from_slice(&entries.to_le_bytes());
    end.extend_from_slice(
        &u32::try_from(central_size)
            .map_err(|_| "Центральный каталог не влезает в ZIP.".to_owned())?
            .to_le_bytes(),
    );
    end.extend_from_slice(
        &u32::try_from(central_new_offset)
            .map_err(|_| "Смещение каталога не влезает в ZIP.".to_owned())?
            .to_le_bytes(),
    );
    end.extend_from_slice(
        &u16::try_from(tail.comment.len())
            .map_err(|_| "Комментарий ZIP длиннее 65535 байт.".to_owned())?
            .to_le_bytes(),
    );
    end.extend_from_slice(&tail.comment);
    file.write_all(&end).map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())?;
    Ok(())
}

/// Дописывает комментарий ZIP так, чтобы файл вырос ровно на `len` байт.
///
/// Поле длины комментария в конце архива уже существует, поэтому оно
/// перезаписывается, а в конец уходят только байты комментария.
fn write_zip_comment(path: &Path, len: u64) -> Result<(), String> {
    if len == 0 {
        return Ok(());
    }
    let comment_len =
        u16::try_from(len).map_err(|_| "Комментарий ZIP длиннее 65535 байт.".to_owned())?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    let size = file.metadata().map_err(|err| err.to_string())?.len();
    if size < 22 {
        return Err("Не удалось добрать размер: архив слишком короткий.".to_owned());
    }
    file.seek(SeekFrom::End(-22))
        .map_err(|err| err.to_string())?;
    let mut end = [0_u8; 22];
    file.read_exact(&mut end).map_err(|err| err.to_string())?;
    if &end[..4] != b"PK\x05\x06" {
        return Err("Не удалось добрать размер: конец архива не найден.".to_owned());
    }
    if u16::from_le_bytes([end[20], end[21]]) != 0 {
        return Err("Не удалось добрать размер: у архива уже есть комментарий.".to_owned());
    }
    let bytes = random_bytes(comment_len as usize)?;
    file.seek(SeekFrom::End(-2))
        .map_err(|err| err.to_string())?;
    file.write_all(&comment_len.to_le_bytes())
        .and_then(|_| file.write_all(&bytes))
        .map_err(|err| err.to_string())?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions as FileOptions;
    use zip::CompressionMethod;

    const CONTENT_TYPES_XML: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/",
        "vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/",
        "vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
        "</Types>"
    );

    /// Минимальный пакет OPC, как у docx: типы содержимого, связи и документ.
    fn build_package(path: &Path, extra: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in [
            (CONTENT_TYPES_PART, CONTENT_TYPES_XML.as_bytes()),
            ("_rels/.rels", b"<Relationships/>".as_slice()),
            ("word/document.xml", b"<Document/>".as_slice()),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    /// Части без типа содержимого: именно из-за них Word предлагает
    /// «восстановить» документ.
    fn undeclared_parts(path: &Path) -> Vec<String> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut content_types = String::new();
        archive
            .by_name(CONTENT_TYPES_PART)
            .unwrap()
            .read_to_string(&mut content_types)
            .unwrap();
        let mut missing = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index).unwrap();
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            drop(entry);
            let extension = name
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if content_types.contains(&format!("Extension=\"{extension}\""))
                || content_types.contains(&format!("PartName=\"/{name}\""))
            {
                continue;
            }
            missing.push(name);
        }
        missing
    }

    #[test]
    fn content_type_declaration_is_added_once() {
        let declared = declare_padding_content_type(CONTENT_TYPES_XML.as_bytes()).unwrap();
        let text = String::from_utf8(declared.clone()).unwrap();
        assert_eq!(text.matches("Extension=\"fppad\"").count(), 1);
        assert!(text.contains(PADDING_DECLARATION));
        assert!(text.ends_with("</Types>"));

        let again = declare_padding_content_type(&declared).unwrap();
        assert_eq!(again, declared);
    }

    #[test]
    fn content_type_declaration_reports_broken_packages() {
        assert!(declare_padding_content_type(b"<Types>")
            .unwrap_err()
            .contains("</Types>"));
    }

    #[test]
    fn opc_padding_declares_part_and_hits_exact_size() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("report.docx");
        build_package(&input, &[]);
        let start = fs::metadata(&input).unwrap().len();
        let target = start + 500_000;
        let output = dir.path().join("report_5MB.docx");
        run(&input, &output, target, &Job::quiet()).unwrap();

        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        assert!(undeclared_parts(&output).is_empty());

        let mut archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert!(
            archive.comment().is_empty(),
            "точный размер не требует хвоста"
        );
        assert_eq!(archive.index_for_name(CONTENT_TYPES_PART), Some(0));
        assert!(archive.index_for_name(PADDING_PART).is_some());
        let padding = archive.by_name(PADDING_PART).unwrap().size();
        assert!(padding > 400_000, "заполнитель слишком мал: {padding}");
        // Все части читаются: архив не повреждён пересборкой.
        for index in 0..archive.len() {
            let mut part = archive.by_index(index).unwrap();
            let mut sink = Vec::new();
            part.read_to_end(&mut sink).unwrap();
        }
    }

    #[test]
    fn opc_padding_keeps_parts_and_drops_legacy_padding() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("legacy.docx");
        build_package(&input, &[("word_media_cache.bin", &[7_u8; 4096])]);
        let start = fs::metadata(&input).unwrap().len();
        let output = dir.path().join("legacy_big.docx");
        run(&input, &output, start + 200_000, &Job::quiet()).unwrap();

        let archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert!(archive.index_for_name("word_media_cache.bin").is_none());
        assert!(archive.index_for_name("word/document.xml").is_some());
        assert!(archive.index_for_name("_rels/.rels").is_some());
        drop(archive);
        assert!(undeclared_parts(&output).is_empty());
    }

    #[test]
    fn opc_padding_can_be_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("twice.docx");
        build_package(&input, &[]);
        let first = dir.path().join("twice_1.docx");
        let second = dir.path().join("twice_2.docx");
        let start = fs::metadata(&input).unwrap().len();
        run(&input, &first, start + 300_000, &Job::quiet()).unwrap();
        let middle = fs::metadata(&first).unwrap().len();
        run(&first, &second, middle + 400_000, &Job::quiet()).unwrap();

        assert_eq!(fs::metadata(&second).unwrap().len(), middle + 400_000);
        assert!(undeclared_parts(&second).is_empty());
        let mut archive = ZipArchive::new(File::open(&second).unwrap()).unwrap();
        let mut content_types = String::new();
        archive
            .by_name(CONTENT_TYPES_PART)
            .unwrap()
            .read_to_string(&mut content_types)
            .unwrap();
        assert_eq!(content_types.matches("Extension=\"fppad\"").count(), 1);
    }

    #[test]
    fn small_gap_is_closed_with_zip_comment() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("small.docx");
        build_package(&input, &[]);
        let start = fs::metadata(&input).unwrap().len();
        let target = start + 40;
        let output = dir.path().join("small_40.docx");
        run(&input, &output, target, &Job::quiet()).unwrap();

        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert_eq!(archive.comment().len(), 40);
        // Структура пакета не менялась: лишних частей не появилось.
        assert!(archive.index_for_name(PADDING_PART).is_none());
        drop(archive);
        assert!(undeclared_parts(&output).is_empty());
    }

    #[test]
    fn plain_archive_padding_hits_exact_size() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("data.zip");
        let mut writer = ZipWriter::new(File::create(&input).unwrap());
        writer
            .start_file("hello.txt", FileOptions::default())
            .unwrap();
        writer.write_all(b"hello").unwrap();
        writer.finish().unwrap();
        let start = fs::metadata(&input).unwrap().len();
        let target = start + 100_000;
        let output = dir.path().join("data_big.zip");
        run(&input, &output, target, &Job::quiet()).unwrap();

        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let mut archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert!(archive.index_for_name(PADDING_PART).is_some());
        assert!(archive.by_name(PADDING_PART).unwrap().size() > 90_000);
        assert!(archive.index_for_name("hello.txt").is_some());
    }

    #[test]
    fn plain_archive_comment_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("commented.zip");
        let mut writer = ZipWriter::new(File::create(&input).unwrap());
        writer.set_comment("сборка 42");
        writer
            .start_file("hello.txt", FileOptions::default())
            .unwrap();
        writer.write_all(b"hello").unwrap();
        writer.finish().unwrap();
        let start = fs::metadata(&input).unwrap().len();
        let target = start + 100_000;
        let output = dir.path().join("commented_big.zip");
        run(&input, &output, target, &Job::quiet()).unwrap();

        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        assert_eq!(archive.comment(), "сборка 42".as_bytes());
        assert!(archive.index_for_name(PADDING_PART).is_some());
    }
}
