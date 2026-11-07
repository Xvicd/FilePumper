//! GIF: нагрузка в блоке Comment Extension перед трейлером.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::payload::format::{MAX_STACK_BYTES, PAYLOAD_NOT_FOUND};

/// 255 байт — предел одного подблока GIF.
pub const SUB_BLOCK_LIMIT: usize = 255;
const EXTENSION: u8 = 0x21;
const COMMENT_LABEL: u8 = 0xFE;
const IMAGE_DESCRIPTOR: u8 = 0x2C;
const TRAILER: u8 = 0x3B;

pub fn embed(carrier: &Path, data: &[u8], copy_len: usize, output: &Path) -> Result<(), String> {
    let total_len = fs::metadata(carrier).map_err(|err| err.to_string())?.len();
    if total_len < 14 {
        return Err("Файл слишком мал для GIF.".into());
    }
    let mut source = BufReader::new(File::open(carrier).map_err(|err| err.to_string())?);
    let mut target = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    let mut head = (&mut source).take(total_len - 1);
    io::copy(&mut head, &mut target).map_err(|err| err.to_string())?;
    let mut trailer = [0_u8; 1];
    source
        .read_exact(&mut trailer)
        .map_err(|_| "Повреждённый GIF: обрезанный трейлер.".to_owned())?;
    if trailer[0] != TRAILER {
        return Err("Повреждённый GIF: не найден трейлер.".into());
    }
    // Отдельный комментарий на каждую копию: если файл обрежут,
    // уцелевшие копии останутся целыми.
    for copy in data.chunks(copy_len.max(1)) {
        target
            .write_all(&[EXTENSION, COMMENT_LABEL])
            .map_err(|err| err.to_string())?;
        for part in copy.chunks(SUB_BLOCK_LIMIT) {
            let size =
                u8::try_from(part.len()).map_err(|_| "Слишком длинный блок GIF.".to_owned())?;
            target.write_all(&[size]).map_err(|err| err.to_string())?;
            target.write_all(part).map_err(|err| err.to_string())?;
        }
        target.write_all(&[0x00]).map_err(|err| err.to_string())?;
    }
    target
        .write_all(&[TRAILER])
        .map_err(|err| err.to_string())?;
    target.flush().map_err(|err| err.to_string())
}

fn skip_sub_blocks<R: Read + Seek>(source: &mut R) -> Result<(), String> {
    loop {
        let mut size = [0_u8; 1];
        source
            .read_exact(&mut size)
            .map_err(|_| "Повреждённый GIF: обрезанный блок.".to_owned())?;
        if size[0] == 0 {
            return Ok(());
        }
        source
            .seek(SeekFrom::Current(i64::from(size[0])))
            .map_err(|err| err.to_string())?;
    }
}

pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let mut source = BufReader::new(File::open(carrier).map_err(|err| err.to_string())?);
    let mut header = [0_u8; 13];
    source
        .read_exact(&mut header)
        .map_err(|_| "Файл не является GIF.".to_owned())?;
    if &header[0..4] != b"GIF8" {
        return Err("Файл не является GIF.".into());
    }
    let packed = header[10];
    if packed & 0x80 != 0 {
        let table_size = 3 * (1_u64 << ((packed & 0x07) + 1));
        source
            .seek(SeekFrom::Current(table_size as i64))
            .map_err(|err| err.to_string())?;
    }
    let mut stack = Vec::new();
    'scan: loop {
        let mut block = [0_u8; 1];
        if source.read_exact(&mut block).is_err() {
            // Файл обрезан: отдаём всё, что успели собрать.
            break 'scan;
        }
        match block[0] {
            TRAILER => break,
            EXTENSION => {
                let mut label = [0_u8; 1];
                if source.read_exact(&mut label).is_err() {
                    break 'scan;
                }
                let mut data = Vec::new();
                loop {
                    let mut size = [0_u8; 1];
                    if source.read_exact(&mut size).is_err() {
                        break 'scan;
                    }
                    if size[0] == 0 {
                        break;
                    }
                    let mut chunk = vec![0_u8; usize::from(size[0])];
                    if source.read_exact(&mut chunk).is_err() {
                        break 'scan;
                    }
                    data.extend_from_slice(&chunk);
                    if data.len() + stack.len() > MAX_STACK_BYTES {
                        return Err("Полезная нагрузка превышает допустимый размер.".into());
                    }
                }
                if label[0] == COMMENT_LABEL {
                    stack.extend_from_slice(&data);
                }
            }
            IMAGE_DESCRIPTOR => {
                let mut descriptor = [0_u8; 9];
                if source.read_exact(&mut descriptor).is_err() {
                    break 'scan;
                }
                let flags = descriptor[8];
                if flags & 0x80 != 0 {
                    let table_size = 3 * (1_u64 << ((flags & 0x07) + 1));
                    source
                        .seek(SeekFrom::Current(table_size as i64))
                        .map_err(|err| err.to_string())?;
                }
                let mut min_code = [0_u8; 1];
                if source.read_exact(&mut min_code).is_err()
                    || skip_sub_blocks(&mut source).is_err()
                {
                    break 'scan;
                }
            }
            other => {
                return Err(format!("Повреждённый GIF: неизвестный блок 0x{other:02X}."));
            }
        }
    }
    if stack.is_empty() {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    Ok(stack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_block_limit_matches_gif_spec() {
        assert_eq!(SUB_BLOCK_LIMIT, 255);
    }
}
