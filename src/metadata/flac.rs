//! FLAC: комментарии Vorbis (теги) и картинки обложек.

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::metadata::{copy_exact, open_input, CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

const VORBIS_COMMENT: u8 = 4;
const PICTURE: u8 = 6;

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let total = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let mut source = open_input(input)?;
    let mut progress = Progress::new(total, job);
    let mut magic = [0_u8; 4];
    source
        .read_exact(&mut magic)
        .map_err(|_| "Файл не является FLAC.".to_owned())?;
    if &magic != b"fLaC" {
        return Err("Файл не является FLAC.".to_owned());
    }
    output.write_all(&magic).map_err(|err| err.to_string())?;
    let mut consumed = 4_u64;

    let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
    loop {
        let mut header = [0_u8; 4];
        source
            .read_exact(&mut header)
            .map_err(|_| "Повреждённый FLAC: обрезан блок метаданных.".to_owned())?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let length =
            ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
        let mut data = vec![0_u8; length];
        source
            .read_exact(&mut data)
            .map_err(|_| "Повреждённый FLAC: обрезан блок метаданных.".to_owned())?;
        consumed += 4 + length as u64;
        progress.advance(4 + length as u64)?;
        blocks.push((kind, data));
        if last {
            break;
        }
    }
    if blocks.is_empty() {
        return Err("Повреждённый FLAC: нет блоков метаданных.".to_owned());
    }

    let mut kept: Vec<(u8, Vec<u8>)> = Vec::new();
    for (kind, data) in blocks {
        let removal = if options.text && kind == VORBIS_COMMENT {
            Some("FLAC-комментарий (Vorbis)")
        } else if options.text && kind == PICTURE {
            Some("FLAC-картинка (обложка)")
        } else {
            None
        };
        match removal {
            Some(label) => report
                .removed
                .push(Removal::new(label, 4 + data.len() as u64)),
            None => kept.push((kind, data)),
        }
    }

    let count = kept.len();
    for (index, (kind, data)) in kept.iter().enumerate() {
        let last = index + 1 == count;
        let length = data.len();
        if length > 0x00FF_FFFF {
            return Err("Повреждённый FLAC: блок метаданных слишком большой.".to_owned());
        }
        let header = [
            (if last { 0x80 } else { 0 }) | (kind & 0x7F),
            ((length >> 16) & 0xFF) as u8,
            ((length >> 8) & 0xFF) as u8,
            (length & 0xFF) as u8,
        ];
        output.write_all(&header).map_err(|err| err.to_string())?;
        output.write_all(data).map_err(|err| err.to_string())?;
    }

    if total > consumed {
        copy_exact(&mut source, output, total - consumed, &mut progress)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn flac_sample() -> Vec<u8> {
        let blocks: [(u8, Vec<u8>); 3] = [
            (0, vec![0_u8; 34]),
            (VORBIS_COMMENT, b"FilePumper artist".to_vec()),
            (PICTURE, b"\x89PNG cover".to_vec()),
        ];
        let mut bytes = b"fLaC".to_vec();
        let count = blocks.len();
        for (index, (kind, data)) in blocks.iter().enumerate() {
            let last = index + 1 == count;
            let length = data.len();
            bytes.push((if last { 0x80 } else { 0 }) | (kind & 0x7F));
            bytes.push(((length >> 16) & 0xFF) as u8);
            bytes.push(((length >> 8) & 0xFF) as u8);
            bytes.push((length & 0xFF) as u8);
            bytes.extend_from_slice(data);
        }
        bytes.extend_from_slice(&[0xFF, 0xF8, 1, 2, 3, 4]);
        bytes
    }

    fn read_blocks(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut offset = 4;
        let mut blocks = Vec::new();
        loop {
            let kind = bytes[offset] & 0x7F;
            let last = bytes[offset] & 0x80 != 0;
            let length = ((bytes[offset + 1] as usize) << 16)
                | ((bytes[offset + 2] as usize) << 8)
                | bytes[offset + 3] as usize;
            blocks.push((kind, bytes[offset + 4..offset + 4 + length].to_vec()));
            offset += 4 + length;
            if last {
                break;
            }
        }
        blocks
    }

    #[test]
    fn comments_and_pictures_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.flac");
        fs::write(&input, flac_sample()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Flac);
        let output = dir.path().join("clean.flac");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 2, "{:?}", report.removed);
        let bytes = fs::read(&output).unwrap();
        let blocks = read_blocks(&bytes);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        assert_eq!(blocks[0].0, 0);
        assert!(bytes.ends_with(&[0xFF, 0xF8, 1, 2, 3, 4]));
        assert!(!bytes.windows(14).any(|window| window == b"FilePumper art"));
    }

    #[test]
    fn keeping_options_keeps_tags() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("song.flac");
        fs::write(&input, flac_sample()).unwrap();
        let output = dir.path().join("keep.flac");
        let options = CleanOptions {
            text: false,
            profiles: false,
        };
        let report = clean(&input, &output, options, &Job::quiet()).unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(fs::read(&output).unwrap(), flac_sample());
    }
}
