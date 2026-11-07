//! Файлы без поддерживаемой структуры: контейнер дописывается в конец,
//! а концевой маркер позволяет найти его даже после обрезки хвоста сервером.

use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::payload::format::{
    CONTAINER_FOOTER, CONTAINER_MAGIC, MAX_CONTAINER_BYTES, PAYLOAD_NOT_FOUND,
};

/// Сколько последних байт просматривается, если концевой маркер утрачен.
pub const TAIL_SCAN_BYTES: u64 = 64 * 1024 * 1024;

pub fn embed(carrier: &Path, data: &[u8], output: &Path) -> Result<(), String> {
    fs::copy(carrier, output).map_err(|err| err.to_string())?;
    let mut file = File::options()
        .append(true)
        .open(output)
        .map_err(|err| err.to_string())?;
    file.write_all(data).map_err(|err| err.to_string())?;
    file.flush().map_err(|err| err.to_string())
}

fn find_magic(haystack: &[u8]) -> Option<usize> {
    haystack
        .windows(CONTAINER_MAGIC.len())
        .position(|window| window == CONTAINER_MAGIC)
}

/// 1) Сначала читаем хвост файла: там находятся все копии контейнера.
/// 2) Если файл больше окна, идём по концевому маркеру.
pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(carrier).map_err(|err| err.to_string())?;
    let total_len = file.metadata().map_err(|err| err.to_string())?.len();
    if total_len < 24 {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }

    let window = total_len.min(TAIL_SCAN_BYTES);
    file.seek(SeekFrom::Start(total_len - window))
        .map_err(|err| err.to_string())?;
    let mut tail = Vec::with_capacity(window as usize);
    BufReader::new(file)
        .take(window)
        .read_to_end(&mut tail)
        .map_err(|err| err.to_string())?;
    match find_magic(&tail) {
        Some(start) => Ok(tail.split_off(start)),
        None => find_by_footer(carrier, total_len),
    }
}

fn find_by_footer(carrier: &Path, total_len: u64) -> Result<Vec<u8>, String> {
    let mut file = File::open(carrier).map_err(|err| err.to_string())?;
    file.seek(SeekFrom::End(-12))
        .map_err(|err| err.to_string())?;
    let mut footer = [0_u8; 12];
    if file.read_exact(&mut footer).is_err() || &footer[0..4] != CONTAINER_FOOTER {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    let container_len = u64::from_be_bytes([
        footer[4], footer[5], footer[6], footer[7], footer[8], footer[9], footer[10], footer[11],
    ]);
    if !(24..=total_len).contains(&container_len) || container_len > MAX_CONTAINER_BYTES as u64 * 3
    {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    file.seek(SeekFrom::Start(total_len - container_len))
        .map_err(|err| err.to_string())?;
    let mut container = vec![0_u8; container_len as usize];
    file.read_exact(&mut container)
        .map_err(|_| "Полезная нагрузка обрезана.".to_owned())?;
    Ok(container)
}
