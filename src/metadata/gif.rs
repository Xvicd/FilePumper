//! GIF: комментарии, текстовые блоки и XMP-расширения.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use crate::metadata::{
    copy_exact, open_input, skip_exact, CleanOptions, Progress, Removal, Report, SeekWrite,
};
use crate::util::Job;

const EXTENSION: u8 = 0x21;
const COMMENT: u8 = 0xFE;
const APPLICATION: u8 = 0xFF;
const PLAIN_TEXT: u8 = 0x01;
const IMAGE: u8 = 0x2C;
const TRAILER: u8 = 0x3B;
/// Идентификатор приложения в XMP-расширении GIF.
const XMP_ID: &[u8] = b"XMP DataXMP";

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

    let mut header = [0_u8; 6];
    source
        .read_exact(&mut header)
        .map_err(|_| "Файл не является GIF.".to_owned())?;
    if !header.starts_with(b"GIF8") {
        return Err("Файл не является GIF.".to_owned());
    }
    output.write_all(&header).map_err(|err| err.to_string())?;
    progress.advance(6)?;

    let mut descriptor = [0_u8; 7];
    source
        .read_exact(&mut descriptor)
        .map_err(|_| "Повреждённый GIF: нет описания экрана.".to_owned())?;
    output
        .write_all(&descriptor)
        .map_err(|err| err.to_string())?;
    progress.advance(7)?;
    if descriptor[4] & 0x80 != 0 {
        let table = 3_u64 << ((descriptor[4] & 0x07) + 1);
        copy_exact(&mut source, output, table, &mut progress)?;
    }

    loop {
        let introducer = read_byte(&mut source)?;
        progress.advance(1)?;
        match introducer {
            EXTENSION => clean_extension(&mut source, output, options, report, &mut progress)?,
            IMAGE => {
                output.write_all(&[IMAGE]).map_err(|err| err.to_string())?;
                let mut image = [0_u8; 9];
                source
                    .read_exact(&mut image)
                    .map_err(|_| "Повреждённый GIF: обрезан кадр.".to_owned())?;
                output.write_all(&image).map_err(|err| err.to_string())?;
                progress.advance(9)?;
                if image[8] & 0x80 != 0 {
                    let table = 3_u64 << ((image[8] & 0x07) + 1);
                    copy_exact(&mut source, output, table, &mut progress)?;
                }
                // Байт размера кода LZW и далее подблоки со сжатыми данными.
                let code_size = read_byte(&mut source)?;
                progress.advance(1)?;
                output
                    .write_all(&[code_size])
                    .map_err(|err| err.to_string())?;
                copy_sub_blocks(&mut source, Some(output), &mut progress)?;
            }
            TRAILER => {
                output
                    .write_all(&[TRAILER])
                    .map_err(|err| err.to_string())?;
                let mut tail = Vec::new();
                source
                    .read_to_end(&mut tail)
                    .map_err(|err| err.to_string())?;
                output.write_all(&tail).map_err(|err| err.to_string())?;
                return Ok(());
            }
            other => return Err(format!("Повреждённый GIF: неизвестный блок 0x{other:02X}.")),
        }
    }
}

fn clean_extension(
    source: &mut impl Read,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    progress: &mut Progress<'_, '_>,
) -> Result<(), String> {
    let label = read_byte(source)?;
    progress.advance(1)?;
    let size = read_byte(source)?;
    progress.advance(1)?;
    let mut header = vec![0_u8; size as usize];
    source
        .read_exact(&mut header)
        .map_err(|_| "Повреждённый GIF: обрезано расширение.".to_owned())?;
    progress.advance(size as u64)?;

    let removal = if options.text {
        match label {
            COMMENT => Some("GIF комментарий".to_owned()),
            PLAIN_TEXT => Some("GIF текстовый блок".to_owned()),
            APPLICATION if header.starts_with(XMP_ID) => Some("GIF XMP".to_owned()),
            _ => None,
        }
    } else {
        None
    };

    match removal {
        Some(label) => {
            let body = copy_sub_blocks(source, None, progress)?;
            report
                .removed
                .push(Removal::new(label, 2 + size as u64 + body));
        }
        None => {
            output
                .write_all(&[EXTENSION, label, size])
                .and_then(|_| output.write_all(&header))
                .map_err(|err| err.to_string())?;
            copy_sub_blocks(source, Some(output), progress)?;
        }
    }
    Ok(())
}

/// Копирует подблоки GIF (размер, данные, …) до нулевого размера.
///
/// Возвращает число скопированных байт вместе с нулевым терминатором.
fn copy_sub_blocks(
    source: &mut impl Read,
    mut output: Option<&mut dyn SeekWrite>,
    progress: &mut Progress<'_, '_>,
) -> Result<u64, String> {
    let mut total = 0_u64;
    loop {
        let size = read_byte(source)?;
        progress.advance(1)?;
        total += 1;
        if let Some(writer) = output.as_mut() {
            writer.write_all(&[size]).map_err(|err| err.to_string())?;
        }
        if size == 0 {
            return Ok(total);
        }
        match output.as_mut() {
            Some(writer) => copy_exact(source, *writer, u64::from(size), progress)?,
            None => skip_exact(source, u64::from(size), progress)?,
        }
        total += u64::from(size);
    }
}

fn read_byte(source: &mut impl Read) -> Result<u8, String> {
    let mut byte = [0_u8; 1];
    source
        .read_exact(&mut byte)
        .map_err(|_| "Повреждённый GIF: файл обрезан.".to_owned())?;
    Ok(byte[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    fn sample_gif() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Gif,
            )
            .unwrap();
        let mut with_meta = Vec::new();
        let trailer = bytes.pop().unwrap();
        assert_eq!(trailer, TRAILER);
        with_meta.extend_from_slice(&bytes);
        // Comment Extension: «FilePumper comment».
        with_meta.extend_from_slice(&[EXTENSION, COMMENT, 18]);
        with_meta.extend_from_slice(b"FilePumper comment");
        with_meta.push(0);
        // XMP Application Extension.
        with_meta.extend_from_slice(&[EXTENSION, APPLICATION, 11]);
        with_meta.extend_from_slice(XMP_ID);
        with_meta.extend_from_slice(&[4]);
        with_meta.extend_from_slice(b"<x/>");
        with_meta.push(0);
        with_meta.push(TRAILER);
        with_meta
    }

    #[test]
    fn comments_and_xmp_are_removed_gif_still_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("anim.gif");
        fs::write(&input, sample_gif()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Gif);

        let output = dir.path().join("clean.gif");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 2, "{:?}", report.removed);

        let bytes = fs::read(&output).unwrap();
        assert!(!bytes
            .windows(18)
            .any(|window| window == b"FilePumper comment"));
        assert!(!bytes.windows(11).any(|window| window == XMP_ID));
        assert_eq!(*bytes.last().unwrap(), TRAILER);
        assert!(image::load_from_memory(&bytes).is_ok());
        assert!(report.size_after < report.size_before);
    }
}
