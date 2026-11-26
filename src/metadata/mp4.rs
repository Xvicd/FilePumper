//! MP4/MOV: служебные боксы `udta` (теги, обложки, служебные пометки).

use std::fs;
use std::path::Path;

use crate::metadata::{CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

/// Контейнерные боксы: внутри них лежат другие боксы.
const CONTAINERS: [&[u8; 4]; 9] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"mvex", b"moof",
];

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let total = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let data = fs::read(input).map_err(|err| err.to_string())?;
    let mut progress = Progress::new(total, job);
    if data.len() < 8 || &data[4..8] != b"ftyp" {
        return Err("Файл не является MP4/MOV.".to_owned());
    }
    if !options.text {
        output.write_all(&data).map_err(|err| err.to_string())?;
        progress.advance(total)?;
        return Ok(());
    }
    let mut cleaned = Vec::with_capacity(data.len());
    let mut removed = 0_u64;
    rewrite_boxes(&data, 0, data.len(), &mut cleaned, &mut removed)?;
    if removed > 0 {
        report
            .removed
            .push(Removal::new("MP4/MOV udta (теги и обложки)", removed));
    }
    output.write_all(&cleaned).map_err(|err| err.to_string())?;
    progress.advance(total)?;
    Ok(())
}

/// Переписывает боксы, выбрасывая `udta`; остальные копирует как есть.
///
/// Размеры контейнеров пересчитываются: после удаления `udta` старое значение
/// в заголовке стало бы неверным и файл перестал бы разбираться.
fn rewrite_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut Vec<u8>,
    removed: &mut u64,
) -> Result<(), String> {
    let mut position = start;
    while position + 8 <= end {
        let size32 = u32::from_be_bytes([
            data[position],
            data[position + 1],
            data[position + 2],
            data[position + 3],
        ]);
        let kind = &data[position + 4..position + 8];
        let (size, header) = match size32 {
            1 => {
                if position + 16 > end {
                    break;
                }
                let large = u64::from_be_bytes([
                    data[position + 8],
                    data[position + 9],
                    data[position + 10],
                    data[position + 11],
                    data[position + 12],
                    data[position + 13],
                    data[position + 14],
                    data[position + 15],
                ]);
                (large, 16_usize)
            }
            0 => ((end - position) as u64, 8_usize),
            value => (u64::from(value), 8_usize),
        };
        let body = position + header;
        let body_end = position
            .checked_add(usize::try_from(size).map_err(|_| "Бокс MP4 слишком большой.".to_owned())?)
            .unwrap_or(end);
        if size < header as u64 || body_end > end {
            return Err("Повреждённый MP4: размер бокса выходит за файл.".to_owned());
        }
        if kind == b"udta" {
            *removed += size;
            position = body_end;
            continue;
        }
        let mut content = Vec::new();
        if CONTAINERS.iter().any(|container| *container == kind) {
            rewrite_boxes(data, body, body_end, &mut content, removed)?;
        } else if kind == b"meta" {
            // Full box: 4 байта версии и флагов, затем вложенные боксы.
            if body + 4 <= body_end {
                content.extend_from_slice(&data[body..body + 4]);
                rewrite_boxes(data, body + 4, body_end, &mut content, removed)?;
            }
        } else {
            content.extend_from_slice(&data[body..body_end]);
        }
        if size32 == 0 {
            // Бокс «до конца файла»: оставляем исходный заголовок как есть.
            out.extend_from_slice(&data[position..body]);
        } else if header == 16 {
            out.extend_from_slice(&1_u32.to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(&((content.len() + header) as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&((content.len() + header) as u32).to_be_bytes());
            out.extend_from_slice(kind);
        }
        out.extend_from_slice(&content);
        position = body_end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn box_(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn sample_mp4() -> Vec<u8> {
        let mut moov = Vec::new();
        moov.extend_from_slice(&box_(b"mvhd", b"header"));
        let mut trak = Vec::new();
        trak.extend_from_slice(&box_(b"tkhd", b"track"));
        moov.extend_from_slice(&box_(b"trak", &trak));
        let mut udta = Vec::new();
        udta.extend_from_slice(&box_(b"meta", b"\x00\x00\x00\x00ilst"));
        moov.extend_from_slice(&box_(b"udta", &udta));
        let mut bytes = box_(b"ftyp", b"isom\x00\x00\x00\x00");
        bytes.extend_from_slice(&box_(b"moov", &moov));
        bytes.extend_from_slice(&box_(b"mdat", &[1, 2, 3, 4]));
        bytes
    }

    fn kinds(bytes: &[u8]) -> Vec<(String, u64)> {
        let mut result = Vec::new();
        let mut position = 0;
        while position + 8 <= bytes.len() {
            let size = u32::from_be_bytes([
                bytes[position],
                bytes[position + 1],
                bytes[position + 2],
                bytes[position + 3],
            ]) as usize;
            if size < 8 || position + size > bytes.len() {
                break;
            }
            result.push((
                String::from_utf8_lossy(&bytes[position + 4..position + 8]).into_owned(),
                size as u64,
            ));
            position += size;
        }
        result
    }

    #[test]
    fn udta_is_removed_and_other_boxes_stay() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clip.mp4");
        let source = sample_mp4();
        fs::write(&input, &source).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Mp4);
        let output = dir.path().join("clean.mp4");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
        let bytes = fs::read(&output).unwrap();
        assert!(!bytes.windows(4).any(|window| window == b"udta"));
        assert!(bytes.windows(4).any(|window| window == b"mvhd"));
        assert!(bytes.windows(4).any(|window| window == b"trak"));
        let top = kinds(&bytes);
        assert_eq!(top.len(), 3, "{top:?}");
        assert_eq!(top[0].0, "ftyp");
        assert!(bytes.len() < source.len());
    }

    #[test]
    fn without_udta_file_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("plain.mp4");
        let mut source = box_(b"ftyp", b"isom\x00\x00\x00\x00");
        source.extend_from_slice(&box_(b"mdat", &[9, 9]));
        fs::write(&input, &source).unwrap();
        let output = dir.path().join("clean.mp4");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(fs::read(&output).unwrap(), source);
    }
}
