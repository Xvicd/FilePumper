//! WebP: нагрузка в отдельном RIFF-чанке `UFP1`, размер RIFF пересчитывается,
//! поэтому файл остаётся полностью валидным для декодеров и серверов.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::payload::format::{MAX_STACK_BYTES, PAYLOAD_NOT_FOUND};

pub const CHUNK_ID: &[u8; 4] = b"UFP1";

pub fn embed(carrier: &Path, data: &[u8], copy_len: usize, output: &Path) -> Result<(), String> {
    let total = fs::metadata(carrier).map_err(|err| err.to_string())?.len();
    if total < 12 {
        return Err("Файл слишком мал для WebP.".into());
    }
    let mut source = BufReader::new(File::open(carrier).map_err(|err| err.to_string())?);
    let mut head = [0_u8; 12];
    source
        .read_exact(&mut head)
        .map_err(|_| "Файл не является WebP.".to_owned())?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return Err("Файл не является WebP.".into());
    }
    // Сколько байт занимает сам RIFF по заголовку: всё, что лежит дальше,
    // считается «хвостом» (например, случайные данные генератора) и в RIFF
    // не попадает, иначе декодеры споткнутся о мусор в середине файла.
    let declared = 8 + u64::from(u32::from_le_bytes([head[4], head[5], head[6], head[7]]));
    let valid_end = declared.clamp(12, total);

    let mut target = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    target.write_all(&head).map_err(|err| err.to_string())?;
    let mut rest = source.by_ref().take(valid_end - 12);
    std::io::copy(&mut rest, &mut target).map_err(|err| err.to_string())?;

    // Отдельный RIFF-чанк на каждую копию.
    for copy in data.chunks(copy_len.max(1)) {
        let chunk_len = u32::try_from(copy.len())
            .map_err(|_| "Полезная нагрузка слишком велика для WebP.".to_owned())?;
        target
            .write_all(CHUNK_ID)
            .and_then(|_| target.write_all(&chunk_len.to_le_bytes()))
            .and_then(|_| target.write_all(copy))
            .map_err(|err| err.to_string())?;
        if copy.len() % 2 == 1 {
            target.write_all(&[0]).map_err(|err| err.to_string())?;
        }
    }
    let written = target.stream_position().map_err(|err| err.to_string())?;
    let riff_size = u32::try_from(written - 8)
        .map_err(|_| "Итоговый файл слишком велик для WebP.".to_owned())?;

    // Хвост за пределами RIFF переносим как есть: он остаётся вне размера RIFF.
    let mut tail = source.take(total - valid_end);
    std::io::copy(&mut tail, &mut target).map_err(|err| err.to_string())?;

    target.flush().map_err(|err| err.to_string())?;
    drop(target);

    let mut file = File::options()
        .write(true)
        .open(output)
        .map_err(|err| err.to_string())?;
    file.seek(SeekFrom::Start(4))
        .map_err(|err| err.to_string())?;
    file.write_all(&riff_size.to_le_bytes())
        .map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())
}

pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(carrier).map_err(|err| err.to_string())?;
    let total = file.metadata().map_err(|err| err.to_string())?.len();
    let mut source = BufReader::new(file);
    let mut head = [0_u8; 12];
    source
        .read_exact(&mut head)
        .map_err(|_| "Файл не является WebP.".to_owned())?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return Err("Файл не является WebP.".into());
    }
    let mut position = 12_u64;
    let mut stack = Vec::new();
    while position + 8 <= total {
        let mut header = [0_u8; 8];
        if source.read_exact(&mut header).is_err() {
            break;
        }
        let chunk_id = [header[0], header[1], header[2], header[3]];
        let chunk_len = u64::from(u32::from_le_bytes([
            header[4], header[5], header[6], header[7],
        ]));
        let padded = chunk_len + (chunk_len % 2);
        if position + 8 + padded > total {
            // Файл обрезан: возвращаем то, что удалось собрать.
            break;
        }
        if &chunk_id == CHUNK_ID {
            if stack.len() + chunk_len as usize > MAX_STACK_BYTES {
                return Err("Полезная нагрузка превышает допустимый размер.".into());
            }
            let mut part = vec![0_u8; chunk_len as usize];
            let mut pad = [0_u8; 1];
            let read = source.read_exact(&mut part).and_then(|_| {
                if padded > chunk_len {
                    source.read_exact(&mut pad)
                } else {
                    Ok(())
                }
            });
            if read.is_err() {
                break;
            }
            stack.extend_from_slice(&part);
        } else {
            source
                .seek(SeekFrom::Current(padded as i64))
                .map_err(|err| err.to_string())?;
        }
        position += 8 + padded;
    }
    if stack.is_empty() {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    Ok(stack)
}
