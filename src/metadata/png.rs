//! PNG: текстовые чанки, EXIF, время изменения и профили.

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::metadata::{
    copy_exact, open_input, skip_exact, CleanOptions, Progress, Removal, Report, SeekWrite,
};
use crate::payload::carrier::PNG_SIGNATURE;
use crate::util::Job;

/// Сколько байт чанка достаточно, чтобы показать ключевое слово в отчёте.
const LABEL_BYTES: u64 = 256;

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
    let mut signature = [0_u8; 8];
    source
        .read_exact(&mut signature)
        .map_err(|_| "Файл не является PNG.".to_owned())?;
    if signature != PNG_SIGNATURE {
        return Err("Файл не является PNG.".to_owned());
    }
    output
        .write_all(&PNG_SIGNATURE)
        .map_err(|err| err.to_string())?;
    let mut position = 8_u64;
    loop {
        if position + 12 > total {
            return Err("Некорректный PNG: не найден завершающий IEND.".to_owned());
        }
        let mut header = [0_u8; 8];
        source
            .read_exact(&mut header)
            .map_err(|_| "Повреждённый PNG: обрезанный чанк.".to_owned())?;
        let length = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let kind: [u8; 4] = [header[4], header[5], header[6], header[7]];
        if position + 12 + length > total {
            return Err("Повреждённый PNG: чанк выходит за пределы файла.".to_owned());
        }
        let remove_text =
            options.text && matches!(&kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME");
        let remove_profile = options.profiles && &kind == b"iCCP";
        if remove_text || remove_profile {
            let mut prefix = Vec::new();
            if matches!(&kind, b"tEXt" | b"zTXt" | b"iTXt") && length > 0 {
                let take = length.min(LABEL_BYTES);
                let mut buffer = vec![0_u8; take as usize];
                source
                    .read_exact(&mut buffer)
                    .map_err(|_| "Повреждённый PNG: обрезанный чанк.".to_owned())?;
                prefix = buffer;
            }
            let label = removal_label(&kind, &prefix);
            skip_exact(&mut source, length - prefix.len() as u64, &mut progress)?;
            skip_exact(&mut source, 4, &mut progress)?;
            let bytes = 12 + length;
            report.removed.push(Removal::new(label, bytes));
            position += bytes;
            if &kind == b"IEND" {
                break;
            }
            continue;
        }
        output.write_all(&header).map_err(|err| err.to_string())?;
        copy_exact(&mut source, output, length, &mut progress)?;
        copy_exact(&mut source, output, 4, &mut progress)?;
        position += 12 + length;
        if &kind == b"IEND" {
            break;
        }
    }
    // Хвост после IEND (например, заполнение FilePumper) сохраняется как есть.
    let mut tail = Vec::new();
    source
        .read_to_end(&mut tail)
        .map_err(|err| err.to_string())?;
    output.write_all(&tail).map_err(|err| err.to_string())?;
    Ok(())
}

fn removal_label(kind: &[u8; 4], prefix: &[u8]) -> String {
    match kind {
        b"tEXt" | b"zTXt" | b"iTXt" => {
            let keyword = prefix
                .split(|byte| *byte == 0)
                .next()
                .map(|value| String::from_utf8_lossy(value).trim().to_owned())
                .unwrap_or_default();
            let kind_name = String::from_utf8_lossy(kind).to_ascii_lowercase();
            if keyword.is_empty() {
                format!("PNG {kind_name}")
            } else {
                format!("PNG {kind_name} «{keyword}»")
            }
        }
        b"eXIf" => "PNG EXIF".to_owned(),
        b"tIME" => "PNG время изменения".to_owned(),
        b"iCCP" => "PNG цветовой профиль (ICC)".to_owned(),
        other => format!("PNG {}", String::from_utf8_lossy(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, scan, CleanOptions, Kind};
    use std::fs::File;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(kind);
        hasher.update(data);
        bytes.extend_from_slice(&hasher.finalize().to_be_bytes());
        bytes
    }

    fn sample_png() -> Vec<u8> {
        // Настоящее изображение, чтобы проверить, что после чистки оно
        // по-прежнему декодируется.
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([9, 40, 90]));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        // Сигнатура и IHDR: 8 + 4 + 4 + 13 + 4.
        let after_ihdr = 8 + 12 + 13;
        let mut bytes = png[..after_ihdr].to_vec();
        bytes.extend(chunk(b"tEXt", b"Software\0FilePumper"));
        bytes.extend(chunk(b"tIME", &[0x07, 0xE8, 1, 2, 3, 4, 5]));
        bytes.extend(chunk(b"iCCP", b"profile\0\0fake"));
        bytes.extend(chunk(b"caBX", b"private padding"));
        bytes.extend_from_slice(&png[after_ihdr..]);
        bytes
    }

    fn write(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join("sample.png");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn text_chunks_are_removed_and_private_chunks_kept() {
        let dir = tempfile::tempdir().unwrap();
        let input = write(dir.path(), &sample_png());
        assert_eq!(detect(&input).unwrap(), Kind::Png);

        let report = scan(&input, CleanOptions::everything()).unwrap();
        assert_eq!(report.removed.len(), 3, "{:?}", report.removed);
        assert!(report
            .removed
            .iter()
            .any(|removal| removal.label.contains("«Software»")));

        let output = dir.path().join("clean.png");
        let done = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(done.kind, Kind::Png);
        // По умолчанию ICC остаётся: меняет цвет на широком охвате.
        assert_eq!(done.removed.len(), 2);

        let bytes = fs::read(&output).unwrap();
        assert!(!bytes.windows(4).any(|window| window == b"tEXt"));
        assert!(!bytes.windows(4).any(|window| window == b"tIME"));
        assert!(bytes.windows(4).any(|window| window == b"iCCP"));
        assert!(bytes.windows(4).any(|window| window == b"caBX"));
        assert!(bytes.windows(4).any(|window| window == b"IEND"));
        // Файл остаётся валидным изображением.
        assert!(image::load_from_memory(&bytes).is_ok());
        assert!(done.size_after < done.size_before);
    }

    #[test]
    fn profile_removal_happens_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let input = write(dir.path(), &sample_png());
        let output = dir.path().join("clean.png");
        let done = clean(
            &input,
            &output,
            CleanOptions {
                text: false,
                profiles: true,
            },
            &Job::quiet(),
        )
        .unwrap();
        assert_eq!(done.removed.len(), 1);
        let bytes = fs::read(&output).unwrap();
        assert!(!bytes.windows(4).any(|window| window == b"iCCP"));
        assert!(bytes.windows(4).any(|window| window == b"tEXt"));
    }

    #[test]
    fn trailing_padding_survives_cleaning() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = sample_png();
        bytes.extend_from_slice(&[0xAB; 512]);
        let input = write(dir.path(), &bytes);
        let output = dir.path().join("clean.png");
        clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        let cleaned = fs::read(&output).unwrap();
        assert!(cleaned.ends_with(&[0xAB; 512]));
        let mut file = File::open(&output).unwrap();
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut sink).unwrap();
        assert_eq!(cleaned, sink);
    }
}
