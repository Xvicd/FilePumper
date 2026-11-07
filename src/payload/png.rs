//! PNG: нагрузка лежит в приватных чанках `ufPl` перед `IEND`.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::payload::carrier::PNG_SIGNATURE;
use crate::payload::format::{MAX_STACK_BYTES, PAYLOAD_NOT_FOUND};
use crate::util::Job;

pub const CHUNK_TYPE: &[u8; 4] = b"ufPl";
/// Максимальный размер одного чанка PNG.
pub const CHUNK_LIMIT: usize = 4 * 1024 * 1024;

fn write_chunk<W: Write>(writer: &mut W, chunk_type: &[u8], data: &[u8]) -> Result<(), String> {
    let length = u32::try_from(data.len()).map_err(|_| "Слишком большой PNG-чанк.".to_owned())?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    let crc = hasher.finalize();
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|_| writer.write_all(chunk_type))
        .and_then(|_| writer.write_all(data))
        .and_then(|_| writer.write_all(&crc.to_be_bytes()))
        .map_err(|err| err.to_string())
}

pub fn embed(
    carrier: &Path,
    data: &[u8],
    copy_len: usize,
    output: &Path,
    job: &Job<'_>,
) -> Result<(), String> {
    let total_len = fs::metadata(carrier).map_err(|err| err.to_string())?.len();
    let mut source = BufReader::new(File::open(carrier).map_err(|err| err.to_string())?);
    let mut target = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    let mut signature = [0_u8; 8];
    source
        .read_exact(&mut signature)
        .map_err(|_| "Файл не является PNG.".to_owned())?;
    if signature != PNG_SIGNATURE {
        return Err("Файл не является PNG.".into());
    }
    target
        .write_all(&PNG_SIGNATURE)
        .map_err(|err| err.to_string())?;

    let parts = crate::payload::segments(data, copy_len, CHUNK_LIMIT);
    let mut position = 8_u64;
    loop {
        job.check()?;
        if position + 12 > total_len {
            return Err("Некорректный PNG: не найден завершающий IEND.".into());
        }
        let mut header = [0_u8; 8];
        source
            .read_exact(&mut header)
            .map_err(|_| "Повреждённый PNG: обрезанный чанк.".to_owned())?;
        let chunk_len = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        if &header[4..8] == b"IEND" {
            for (index, part) in parts.iter().enumerate() {
                job.check()?;
                write_chunk(&mut target, CHUNK_TYPE, part)?;
                job.report(
                    (index + 1) as f32 / parts.len() as f32 * 100.0,
                    "Встраивание полезной нагрузки в PNG-чанки...",
                );
            }
            target.write_all(&header).map_err(|err| err.to_string())?;
            let mut crc = [0_u8; 4];
            source
                .read_exact(&mut crc)
                .map_err(|_| "Повреждённый PNG: обрезанный IEND.".to_owned())?;
            target.write_all(&crc).map_err(|err| err.to_string())?;
            break;
        }
        if position + 8 + chunk_len + 4 > total_len {
            return Err("Повреждённый PNG: чанк выходит за пределы файла.".into());
        }
        target.write_all(&header).map_err(|err| err.to_string())?;
        io::copy(&mut (&mut source).take(chunk_len + 4), &mut target)
            .map_err(|err| err.to_string())?;
        position += 8 + chunk_len + 4;
    }
    // Хвост после IEND (если он был) сохраняем как в исходнике.
    io::copy(&mut source, &mut target).map_err(|err| err.to_string())?;
    target.flush().map_err(|err| err.to_string())
}

pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(carrier).map_err(|err| err.to_string())?;
    let total_len = file.metadata().map_err(|err| err.to_string())?.len();
    let mut source = BufReader::new(file);
    let mut signature = [0_u8; 8];
    source
        .read_exact(&mut signature)
        .map_err(|_| "Файл не является PNG.".to_owned())?;
    if signature != PNG_SIGNATURE {
        return Err("Файл не является PNG.".into());
    }
    let mut position = 8_u64;
    let mut stack = Vec::new();
    while position + 12 <= total_len {
        let mut header = [0_u8; 8];
        if source.read_exact(&mut header).is_err() {
            // Файл обрезан сервером: отдаём всё, что успели собрать.
            break;
        }
        position += 8;
        let chunk_len = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let chunk_type = [header[4], header[5], header[6], header[7]];
        if &chunk_type == b"IEND" {
            break;
        }
        let body_len = chunk_len + 4;
        if position + body_len > total_len {
            break;
        }
        if &chunk_type == CHUNK_TYPE {
            if stack.len() + chunk_len as usize > MAX_STACK_BYTES {
                return Err("Полезная нагрузка превышает допустимый размер.".into());
            }
            let mut part = vec![0_u8; chunk_len as usize];
            let mut crc = [0_u8; 4];
            if source.read_exact(&mut part).is_err() || source.read_exact(&mut crc).is_err() {
                break;
            }
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(&chunk_type);
            hasher.update(&part);
            if hasher.finalize() != u32::from_be_bytes(crc) {
                return Err("Повреждённый PNG: CRC чанка не совпадает.".into());
            }
            stack.extend_from_slice(&part);
        } else {
            source
                .seek(SeekFrom::Current(body_len as i64))
                .map_err(|err| err.to_string())?;
        }
        position += body_len;
    }
    if stack.is_empty() {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    Ok(stack)
}
