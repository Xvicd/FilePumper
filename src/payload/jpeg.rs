//! JPEG: нагрузка в цепочке сегментов APP15 сразу после SOI с номерами
//! последовательности, поэтому порядок восстанавливается при извлечении.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::payload::format::{MAX_STACK_BYTES, PAYLOAD_NOT_FOUND};
use crate::util::Job;

const MARKER: u8 = 0xEF;
const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
/// 65533 байта на данные сегмента минус два байта номера последовательности.
pub const SEGMENT_LIMIT: usize = 65_533 - 2;

pub fn embed(
    carrier: &Path,
    data: &[u8],
    copy_len: usize,
    output: &Path,
    job: &Job<'_>,
) -> Result<(), String> {
    let mut source = BufReader::new(File::open(carrier).map_err(|err| err.to_string())?);
    let mut target = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    let mut soi = [0_u8; 2];
    source
        .read_exact(&mut soi)
        .map_err(|_| "Файл не является JPEG.".to_owned())?;
    if soi != SOI {
        return Err("Файл не является JPEG.".into());
    }
    target.write_all(&SOI).map_err(|err| err.to_string())?;

    let parts: Vec<&[u8]> = crate::payload::segments(data, copy_len, SEGMENT_LIMIT);
    if parts.len() > u16::MAX as usize {
        return Err("Слишком много сегментов: нагрузка не помещается в JPEG.".into());
    }
    for (sequence, part) in parts.iter().enumerate() {
        job.check()?;
        let length = u16::try_from(part.len() + 4)
            .map_err(|_| "Слишком большой сегмент JPEG.".to_owned())?;
        target
            .write_all(&[0xFF, MARKER])
            .and_then(|_| target.write_all(&length.to_be_bytes()))
            .and_then(|_| target.write_all(&(sequence as u16).to_be_bytes()))
            .and_then(|_| target.write_all(part))
            .map_err(|err| err.to_string())?;
        job.report(
            (sequence + 1) as f32 / parts.len() as f32 * 100.0,
            "Встраивание полезной нагрузки в сегменты JPEG...",
        );
    }
    io::copy(&mut source, &mut target).map_err(|err| err.to_string())?;
    target.flush().map_err(|err| err.to_string())
}

pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(carrier).map_err(|err| err.to_string())?;
    let mut source = BufReader::new(file);
    let mut soi = [0_u8; 2];
    source
        .read_exact(&mut soi)
        .map_err(|_| "Файл не является JPEG.".to_owned())?;
    if soi != SOI {
        return Err("Файл не является JPEG.".into());
    }
    let mut stack = Vec::new();
    let mut expected = 0_u16;
    while let Some(marker) = read_marker(&mut source)? {
        if marker == EOI || marker == SOS {
            break;
        }
        // Маркеры без длины: TEM (0x01) и RSTn (0xD0..0xD7).
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let mut length_bytes = [0_u8; 2];
        source
            .read_exact(&mut length_bytes)
            .map_err(|_| "Повреждённый JPEG: обрезанный сегмент.".to_owned())?;
        let length = u16::from_be_bytes(length_bytes);
        if length < 2 {
            return Err("Повреждённый JPEG: неверная длина сегмента.".into());
        }
        let body_len = usize::from(length) - 2;
        if marker != MARKER {
            io::copy(&mut (&mut source).take(body_len as u64), &mut io::sink())
                .map_err(|err| err.to_string())?;
            continue;
        }
        let mut body = vec![0_u8; body_len];
        source
            .read_exact(&mut body)
            .map_err(|_| "Повреждённый JPEG: обрезанный сегмент нагрузки.".to_owned())?;
        if body.len() < 2 {
            continue;
        }
        let sequence = u16::from_be_bytes([body[0], body[1]]);
        if sequence != expected {
            // Чужой сегмент APP15 или потерянная часть цепочки.
            continue;
        }
        if stack.len() + body.len() - 2 > MAX_STACK_BYTES {
            return Err("Полезная нагрузка превышает допустимый размер.".into());
        }
        stack.extend_from_slice(&body[2..]);
        expected = expected.wrapping_add(1);
    }
    if stack.is_empty() {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    Ok(stack)
}

/// Читает маркер, пропуская байты заполнения 0xFF.
fn read_marker<R: Read>(reader: &mut R) -> Result<Option<u8>, String> {
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
        if byte[0] == 0xFF {
            break;
        }
        // Байты-заполнители перед маркером просто пропускаем.
    }
    let mut marker = [0_u8; 1];
    loop {
        if reader.read(&mut marker).map_err(|err| err.to_string())? == 0 {
            return Ok(None);
        }
        if marker[0] != 0xFF {
            return Ok(Some(marker[0]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_limit_fits_into_segment_length_field() {
        let length = SEGMENT_LIMIT + 4;
        assert!(u16::try_from(length).is_ok());
    }
}
