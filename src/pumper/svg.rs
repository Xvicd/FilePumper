//! Режим 4: SVG с изображением в data-URI и «невидимыми» элементами-заполнителями.
//! Base64 кодируется потоково, поэтому даже большие изображения не удваивают память.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::write::EncoderWriter;

use crate::util::{hex_encode, random_bytes, Job, IO_CHUNK};

pub fn run(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    job.report(2.0, "Кодирование изображения для SVG...");
    let mime = match input
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    };
    let mut target_file = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    let mut written = 0_u64;
    let header_prefix = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"800\" height=\"800\">\n<image href=\"data:{mime};base64,"
    );
    target_file
        .write_all(header_prefix.as_bytes())
        .map_err(|err| err.to_string())?;
    written += header_prefix.len() as u64;
    {
        let mut source = File::open(input).map_err(|err| err.to_string())?;
        let mut encoder = EncoderWriter::new(&mut target_file, &STANDARD);
        std::io::copy(&mut source, &mut encoder).map_err(|err| err.to_string())?;
        encoder.finish().map_err(|err| err.to_string())?;
    }
    let suffix = "\" width=\"800\" height=\"800\"/>\n";
    target_file
        .write_all(suffix.as_bytes())
        .map_err(|err| err.to_string())?;
    written += suffix.len() as u64;

    let mut index = 0_u64;
    while written < target {
        job.check()?;
        let noise = random_bytes(IO_CHUNK / 2)?;
        let hex = hex_encode(&noise);
        let element = format!("<path d=\"M0 0\" opacity=\"0\" data-noise=\"{hex}\"/>\n");
        target_file
            .write_all(element.as_bytes())
            .map_err(|err| err.to_string())?;
        written += element.len() as u64;
        index += 1;
        if index & 7 == 0 {
            job.report(
                (written as f64 / target.max(1) as f64 * 100.0) as f32,
                "Создание SVG-элементов...",
            );
        }
    }
    target_file
        .write_all(b"</svg>\n")
        .map_err(|err| err.to_string())?;
    target_file.flush().map_err(|err| err.to_string())
}
