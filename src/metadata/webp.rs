//! WebP: RIFF-чанки EXIF, XMP и ICC с правкой флагов VP8X.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::metadata::{copy_exact, skip_exact, CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

const EXIF: &[u8; 4] = b"EXIF";
const XMP: &[u8; 4] = b"XMP ";
const ICC: &[u8; 4] = b"ICCP";
const VP8X: &[u8; 4] = b"VP8X";

/// Биты флагов VP8X, которые надо снять вместе с чанком.
const FLAG_ICC: u8 = 0x20;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;

struct Chunk {
    id: [u8; 4],
    size: u64,
    data_offset: u64,
}

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let total = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let mut progress = Progress::new(total, job);
    let chunks = read_chunks(input)?;
    let riff_end = chunks
        .last()
        .map(|chunk| chunk.data_offset + chunk.size + (chunk.size & 1))
        .unwrap_or(12);

    // Решаем, что удалить, и какие флаги VP8X снять вместе с этим.
    let mut flags_to_clear = 0_u8;
    let mut removed_ids: Vec<[u8; 4]> = Vec::new();
    for chunk in &chunks {
        let (remove, label, flag) = match &chunk.id {
            EXIF => (options.text, "WebP EXIF", FLAG_EXIF),
            XMP => (options.text, "WebP XMP", FLAG_XMP),
            ICC => (options.profiles, "WebP цветовой профиль (ICC)", FLAG_ICC),
            _ => (false, "", 0),
        };
        if remove {
            flags_to_clear |= flag;
            removed_ids.push(chunk.id);
            report
                .removed
                .push(Removal::new(label, 8 + chunk.size + (chunk.size & 1)));
        }
    }

    let mut source = File::open(input).map_err(|err| err.to_string())?;
    output
        .write_all(b"RIFF")
        .and_then(|_| output.write_all(&0_u32.to_le_bytes()))
        .and_then(|_| output.write_all(b"WEBP"))
        .map_err(|err| err.to_string())?;
    let mut written = 12_u64;

    for chunk in &chunks {
        if removed_ids.contains(&chunk.id) {
            skip_exact(&mut source, chunk.size + (chunk.size & 1), &mut progress)?;
            continue;
        }
        source
            .seek(SeekFrom::Start(chunk.data_offset))
            .map_err(|err| err.to_string())?;
        output
            .write_all(&chunk.id)
            .and_then(|_| output.write_all(&(chunk.size as u32).to_le_bytes()))
            .map_err(|err| err.to_string())?;
        if &chunk.id == VP8X && flags_to_clear != 0 && chunk.size >= 1 {
            let mut body = vec![0_u8; chunk.size as usize];
            source
                .read_exact(&mut body)
                .map_err(|_| "Повреждённый WebP: обрезан VP8X.".to_owned())?;
            body[0] &= !flags_to_clear;
            output.write_all(&body).map_err(|err| err.to_string())?;
            progress.advance(chunk.size)?;
        } else {
            copy_exact(&mut source, output, chunk.size, &mut progress)?;
        }
        if chunk.size & 1 == 1 {
            let mut pad = [0_u8; 1];
            source
                .read_exact(&mut pad)
                .map_err(|_| "Повреждённый WebP: нет байта выравнивания.".to_owned())?;
            output.write_all(&pad).map_err(|err| err.to_string())?;
            progress.advance(1)?;
        }
        written += 8 + chunk.size + (chunk.size & 1);
    }

    // Хвост после RIFF (например, заполнение FilePumper) сохраняется как есть.
    if riff_end < total {
        source
            .seek(SeekFrom::Start(riff_end))
            .map_err(|err| err.to_string())?;
        let mut tail = Vec::new();
        source
            .read_to_end(&mut tail)
            .map_err(|err| err.to_string())?;
        output.write_all(&tail).map_err(|err| err.to_string())?;
    }

    // Размер RIFF считается только по чанкам, хвост в него не входит.
    let riff_size = u32::try_from(written - 8).map_err(|_| "Файл WebP больше 4 ГБ.".to_owned())?;
    output
        .seek(SeekFrom::Start(4))
        .map_err(|err| err.to_string())?;
    output
        .write_all(&riff_size.to_le_bytes())
        .map_err(|err| err.to_string())?;
    output
        .seek(SeekFrom::Start(written))
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// Список чанков: заголовки читаются без копирования данных.
fn read_chunks(input: &Path) -> Result<Vec<Chunk>, String> {
    let total = fs::metadata(input).map_err(|err| err.to_string())?.len();
    if total < 12 {
        return Err("Файл не является WebP.".to_owned());
    }
    let mut file = File::open(input).map_err(|err| err.to_string())?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|_| "Файл не является WebP.".to_owned())?;
    if &header[..4] != b"RIFF" || &header[8..12] != b"WEBP" {
        return Err("Файл не является WebP.".to_owned());
    }
    let riff_size = u64::from(u32::from_le_bytes([
        header[4], header[5], header[6], header[7],
    ]));
    let data_end = (8 + riff_size).min(total);
    let mut chunks = Vec::new();
    let mut position = 12_u64;
    while position + 8 <= data_end {
        file.seek(SeekFrom::Start(position))
            .map_err(|err| err.to_string())?;
        let mut chunk_header = [0_u8; 8];
        file.read_exact(&mut chunk_header)
            .map_err(|_| "Повреждённый WebP: обрезан чанк.".to_owned())?;
        let size = u64::from(u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]));
        let data_offset = position + 8;
        if data_offset + size > total {
            return Err("Повреждённый WebP: чанк выходит за пределы файла.".to_owned());
        }
        chunks.push(Chunk {
            id: [
                chunk_header[0],
                chunk_header[1],
                chunk_header[2],
                chunk_header[3],
            ],
            size,
            data_offset,
        });
        position = data_offset + size + (size & 1);
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_vec();
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        if data.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }

    /// WebP c VP8X, изображением и метаданными: флаги EXIF/XMP/ICC выставлены.
    fn sample_webp() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
        let mut base = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut base),
                image::ImageFormat::WebP,
            )
            .unwrap();
        // У простого WebP после «RIFF size WEBP» сразу идёт VP8L.
        let payload = &base[12..];
        let mut vp8x = vec![FLAG_ICC | FLAG_EXIF | FLAG_XMP];
        vp8x.extend_from_slice(&[0, 0, 0]);
        vp8x.extend_from_slice(&[3, 0, 0]); // ширина-1
        vp8x.extend_from_slice(&[3, 0, 0]); // высота-1
        let mut body = chunk(VP8X, &vp8x);
        body.extend(chunk(EXIF, b"MM\0*\0\0\0\x08"));
        body.extend(chunk(XMP, b"<x:xmpmeta/>"));
        body.extend(chunk(ICC, b"fake profile"));
        body.extend_from_slice(payload);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(&body);
        file
    }

    #[test]
    fn metadata_chunks_and_vp8x_flags_are_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("pic.webp");
        fs::write(&input, sample_webp()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Webp);

        let output = dir.path().join("clean.webp");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        // EXIF и XMP уходят, ICC остаётся по умолчанию.
        assert_eq!(report.removed.len(), 2, "{:?}", report.removed);

        let bytes = fs::read(&output).unwrap();
        assert!(!bytes.windows(4).any(|window| window == EXIF));
        assert!(!bytes.windows(4).any(|window| window == XMP));
        assert!(bytes.windows(4).any(|window| window == ICC));
        // Флаги EXIF и XMP сняты, ICC остался.
        let vp8x_at = bytes
            .windows(4)
            .position(|window| window == VP8X)
            .expect("VP8X на месте");
        let flags = bytes[vp8x_at + 8];
        assert_eq!(flags & FLAG_ICC, FLAG_ICC);
        assert_eq!(flags & FLAG_EXIF, 0);
        assert_eq!(flags & FLAG_XMP, 0);
        // Размер RIFF соответствует содержимому.
        let riff = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        assert_eq!(riff + 8, bytes.len());
        assert!(image::load_from_memory(&bytes).is_ok());
    }
}
