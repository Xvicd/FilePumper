//! MP3: ID3v2 в начале файла, ID3v1 и APE-теги в конце.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::metadata::{
    copy_exact, open_input, skip_exact, CleanOptions, Progress, Removal, Report, SeekWrite,
};
use crate::util::Job;

/// Размер ID3v1-тега в конце файла.
const ID3V1_LEN: u64 = 128;
/// Размер футера (и заголовка) APE-тега.
const APE_PART: u64 = 32;
/// Сколько байтов с конца читаем, чтобы найти хвостовые теги.
const TAIL_LEN: u64 = 1024;

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let total = fs::metadata(input).map_err(|err| err.to_string())?.len();
    if total < 4 {
        return Err("Файл слишком мал для MP3.".to_owned());
    }
    let mut source = open_input(input)?;
    let mut progress = Progress::new(total, job);

    if !options.text {
        copy_exact(&mut source, output, total, &mut progress)?;
        return Ok(());
    }

    // Хвостовые теги: ID3v1 последним, перед ним может стоять APE.
    let tail_start = total.saturating_sub(TAIL_LEN);
    let mut tail = vec![0_u8; (total - tail_start) as usize];
    source
        .seek(SeekFrom::Start(tail_start))
        .map_err(|err| err.to_string())?;
    source
        .read_exact(&mut tail)
        .map_err(|err| err.to_string())?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|err| err.to_string())?;

    let mut tail_removed = 0_u64;
    if tail.len() as u64 >= ID3V1_LEN
        && &tail[tail.len() - ID3V1_LEN as usize..tail.len() - ID3V1_LEN as usize + 3] == b"TAG"
    {
        tail_removed += ID3V1_LEN;
        report
            .removed
            .push(Removal::new("MP3 ID3v1 (теги)", ID3V1_LEN));
    }
    let ape_end = total - tail_removed;
    if let Some(footer) = slice_tail(total, tail_start, &tail, ape_end - APE_PART, APE_PART) {
        if &footer[..8] == b"APETAGEX" {
            let size = u32::from_le_bytes([footer[12], footer[13], footer[14], footer[15]]) as u64;
            let flags = u32::from_le_bytes([footer[20], footer[21], footer[22], footer[23]]);
            let header = if flags & 0x8000_0000 != 0 {
                APE_PART
            } else {
                0
            };
            let ape_total = size + APE_PART + header;
            if ape_total <= ape_end {
                tail_removed += ape_total;
                report.removed.push(Removal::new("MP3 APE-теги", ape_total));
            }
        }
    }
    let audio_end = total - tail_removed;

    // Начало: цепочка ID3v2-тегов (накачка может писать их несколько).
    let mut consumed = 0_u64;
    loop {
        if audio_end - consumed < 10 {
            break;
        }
        let mut head = [0_u8; 10];
        source
            .read_exact(&mut head)
            .map_err(|err| err.to_string())?;
        consumed += 10;
        progress.advance(10)?;
        if &head[..3] != b"ID3" {
            output.write_all(&head).map_err(|err| err.to_string())?;
            break;
        }
        let size = syncsafe(&head[6..10]) as u64;
        let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
        let rest = size + footer;
        if consumed + rest > audio_end {
            return Err("ID3v2-тег в MP3 обрезан.".to_owned());
        }
        skip_exact(&mut source, rest, &mut progress)?;
        consumed += rest;
        report
            .removed
            .push(Removal::new("MP3 ID3v2 (теги)", 10 + rest));
    }

    // Аудиоданные: всё до хвостовых тегов.
    if audio_end > consumed {
        copy_exact(&mut source, output, audio_end - consumed, &mut progress)?;
    }
    Ok(())
}

fn syncsafe(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 21)
        | ((bytes[1] as u32) << 14)
        | ((bytes[2] as u32) << 7)
        | bytes[3] as u32
}

/// Байты из уже прочитанного «хвоста»: `start..start+len` в координатах файла.
fn slice_tail(total: u64, tail_start: u64, tail: &[u8], start: u64, len: u64) -> Option<&[u8]> {
    if start < tail_start || start + len > total {
        return None;
    }
    let from = (start - tail_start) as usize;
    tail.get(from..from + len as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn sample_mp3() -> Vec<u8> {
        let mut bytes = Vec::new();
        // ID3v2.3 с заголовком «Bolshoe imya» в теле и нулевым размером.
        let body = b"TIT2\x00\x00\x00\x0bFilePumper\x00";
        bytes.extend_from_slice(b"ID3\x03\x00\x00");
        bytes.extend_from_slice(&syncsafe_bytes(body.len()));
        bytes.extend_from_slice(body);
        // Пара «аудиофреймов».
        bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64, 1, 2, 3, 4, 5]);
        // ID3v1 в конце.
        let mut tag = [0_u8; 128];
        tag[..3].copy_from_slice(b"TAG");
        tag[3..11].copy_from_slice(b"Title01\x00");
        bytes.extend_from_slice(&tag);
        bytes
    }

    fn syncsafe_bytes(value: usize) -> [u8; 4] {
        [
            ((value >> 21) & 0x7F) as u8,
            ((value >> 14) & 0x7F) as u8,
            ((value >> 7) & 0x7F) as u8,
            (value & 0x7F) as u8,
        ]
    }

    #[test]
    fn id3_tags_are_removed_and_audio_stays() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.mp3");
        fs::write(&input, sample_mp3()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Mp3);
        let output = dir.path().join("clean.mp3");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 2, "{:?}", report.removed);
        let bytes = fs::read(&output).unwrap();
        assert!(bytes.windows(3).all(|window| window != b"ID3"));
        assert!(!bytes.windows(3).any(|window| window == b"TAG"));
        assert!(bytes.ends_with(&[0xFF, 0xFB, 0x90, 0x64, 1, 2, 3, 4, 5]));
        assert_eq!(report.size_after, bytes.len() as u64);
    }

    #[test]
    fn ape_tag_is_removed_too() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.mp3");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64, 9, 9, 9]);
        // APE-тег: футер + 8 байтов «элементов».
        let items = 8_u32;
        let mut footer = Vec::new();
        footer.extend_from_slice(b"APETAGEX");
        footer.extend_from_slice(&2000_u32.to_le_bytes());
        footer.extend_from_slice(&items.to_le_bytes());
        footer.extend_from_slice(&0_u32.to_le_bytes());
        footer.extend_from_slice(&0_u32.to_le_bytes());
        footer.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&footer);
        fs::write(&input, &bytes).unwrap();
        let output = dir.path().join("clean.mp3");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
        let result = fs::read(&output).unwrap();
        assert!(!result.windows(8).any(|window| window == b"APETAGEX"));
        assert!(result.ends_with(&[0xFF, 0xFB, 0x90, 0x64, 9, 9, 9]));
    }
}
