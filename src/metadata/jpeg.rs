//! JPEG: EXIF, XMP, IPTC, комментарии и профили в сегментах APPn.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use crate::metadata::{open_input, read_block, CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const COM: u8 = 0xFE;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP13: u8 = 0xED;

/// Сигнатуры содержимого сегментов, по которым видно, что внутри.
const EXIF: &[u8] = b"Exif\0\0";
const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const ICC: &[u8] = b"ICC_PROFILE\0";
const IPTC: &[u8] = b"Photoshop 3.0\0";

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
    let mut soi = [0_u8; 2];
    source
        .read_exact(&mut soi)
        .map_err(|_| "Файл не является JPEG.".to_owned())?;
    if soi != SOI {
        return Err("Файл не является JPEG.".to_owned());
    }
    output.write_all(&SOI).map_err(|err| err.to_string())?;
    progress.advance(2)?;

    loop {
        // Перед маркером допускаются байты заполнения 0xFF.
        let marker = read_marker(&mut source)?;
        progress.advance(2)?;
        let Some(marker) = marker else {
            return Err("Повреждённый JPEG: не найден конец изображения.".to_owned());
        };
        if marker == EOI {
            output
                .write_all(&[0xFF, EOI])
                .map_err(|err| err.to_string())?;
            copy_rest(&mut source, output, &mut progress)?;
            return Ok(());
        }
        // Маркеры без длины: TEM и рестарт-маркеры.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            output
                .write_all(&[0xFF, marker])
                .map_err(|err| err.to_string())?;
            continue;
        }
        let mut length_bytes = [0_u8; 2];
        source
            .read_exact(&mut length_bytes)
            .map_err(|_| "Повреждённый JPEG: обрезанный сегмент.".to_owned())?;
        let length = u16::from_be_bytes(length_bytes);
        if length < 2 {
            return Err("Повреждённый JPEG: неверная длина сегмента.".to_owned());
        }
        let body_len = u64::from(length) - 2;
        let body = read_block(&mut source, body_len)?;
        progress.advance(body_len)?;

        let removal = removal_label(marker, &body, options);
        if let Some(label) = removal {
            report.removed.push(Removal::new(label, 4 + body_len));
        } else {
            output
                .write_all(&[0xFF, marker])
                .and_then(|_| output.write_all(&length_bytes))
                .and_then(|_| output.write_all(&body))
                .map_err(|err| err.to_string())?;
        }

        if marker == SOS {
            // Дальше идут сами сжатые данные и EOI: их не трогаем.
            copy_rest(&mut source, output, &mut progress)?;
            return Ok(());
        }
    }
}

fn read_marker<R: Read>(source: &mut R) -> Result<Option<u8>, String> {
    let mut byte = [0_u8; 1];
    loop {
        match source.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
        if byte[0] == 0xFF {
            break;
        }
    }
    loop {
        if source.read(&mut byte).map_err(|err| err.to_string())? == 0 {
            return Ok(None);
        }
        if byte[0] != 0xFF {
            return Ok(Some(byte[0]));
        }
    }
}

fn removal_label(marker: u8, body: &[u8], options: CleanOptions) -> Option<String> {
    let starts = |signature: &[u8]| body.starts_with(signature);
    match marker {
        APP1 if starts(EXIF) && options.text => Some("JPEG EXIF".to_owned()),
        APP1 if starts(XMP) && options.text => Some("JPEG XMP".to_owned()),
        APP13 if starts(IPTC) && options.text => Some("JPEG IPTC".to_owned()),
        COM if options.text => Some("JPEG комментарий".to_owned()),
        APP2 if starts(ICC) && options.profiles => Some("JPEG цветовой профиль (ICC)".to_owned()),
        _ => None,
    }
}

fn copy_rest<R: Read, W: Write + ?Sized>(
    source: &mut R,
    output: &mut W,
    progress: &mut Progress<'_, '_>,
) -> Result<(), String> {
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(());
        }
        output
            .write_all(&buffer[..read])
            .map_err(|err| err.to_string())?;
        progress.advance(read as u64)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xFF, marker];
        bytes.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn sample_jpeg() -> Vec<u8> {
        let mut image = image::RgbImage::from_pixel(8, 8, image::Rgb([30, 60, 90]));
        image.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // Сегменты метаданных вставляются сразу после SOI.
        let mut with_meta = vec![0xFF, 0xD8];
        with_meta.extend(segment(APP1, b"Exif\0\0MM\0*\0\0\0\x08\0\0"));
        with_meta.extend(segment(APP1, b"http://ns.adobe.com/xap/1.0/\0<xmp/>"));
        with_meta.extend(segment(APP2, b"ICC_PROFILE\0\0\x01profile"));
        with_meta.extend(segment(COM, b"FilePumper comment"));
        with_meta.extend_from_slice(&bytes[2..]);
        with_meta
    }

    #[test]
    fn metadata_segments_are_removed_and_image_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("photo.jpg");
        fs::write(&input, sample_jpeg()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Jpeg);

        let output = dir.path().join("clean.jpg");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.kind, Kind::Jpeg);
        // EXIF, XMP и комментарий уходят, ICC по умолчанию остаётся.
        assert_eq!(report.removed.len(), 3, "{:?}", report.removed);

        let bytes = fs::read(&output).unwrap();
        assert!(!bytes.windows(6).any(|window| window == b"Exif\0\0"));
        assert!(!bytes.windows(4).any(|window| window == b"<xmp"));
        assert!(!bytes
            .windows(18)
            .any(|window| window == b"FilePumper comment"));
        assert!(bytes.windows(11).any(|window| window == b"ICC_PROFILE"));
        assert!(image::load_from_memory(&bytes).is_ok());
        assert!(report.size_after < report.size_before);
    }
}
