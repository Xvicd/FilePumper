//! SVG: нагрузка в элементе `<metadata id="ufpl">` перед `</svg>`,
//! base64 кодируется потоково, чтобы не держать в памяти три копии данных.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::write::EncoderWriter;
use base64::Engine;

use crate::payload::format::PAYLOAD_NOT_FOUND;

pub const METADATA_OPEN: &str = "<metadata id=\"ufpl\">";
pub const METADATA_PREFIX: &str = "<metadata id=\"ufpl";
pub const METADATA_CLOSE: &str = "</metadata>";

pub fn embed(carrier: &Path, data: &[u8], copy_len: usize, output: &Path) -> Result<(), String> {
    let text =
        fs::read_to_string(carrier).map_err(|err| format!("Не удалось прочитать SVG: {err}"))?;
    if text.contains(METADATA_PREFIX) {
        return Err("В этом SVG уже есть полезная нагрузка.".into());
    }
    let insert_at = text
        .rfind("</svg>")
        .ok_or_else(|| "В SVG не найден закрывающий тег </svg>.".to_owned())?;
    let mut target = BufWriter::new(File::create(output).map_err(|err| err.to_string())?);
    target
        .write_all(&text.as_bytes()[..insert_at])
        .map_err(|err| err.to_string())?;
    // Отдельный <metadata> на каждую копию: обрезка файла не уничтожает все копии.
    for (index, copy) in data.chunks(copy_len.max(1)).enumerate() {
        let open = if index == 0 {
            METADATA_OPEN.to_owned()
        } else {
            format!("<metadata id=\"ufpl{}\">", index + 1)
        };
        target
            .write_all(open.as_bytes())
            .map_err(|err| err.to_string())?;
        {
            let mut encoder = EncoderWriter::new(&mut target, &STANDARD);
            encoder.write_all(copy).map_err(|err| err.to_string())?;
            encoder.finish().map_err(|err| err.to_string())?;
        }
        target
            .write_all(METADATA_CLOSE.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    target
        .write_all(&text.as_bytes()[insert_at..])
        .map_err(|err| err.to_string())?;
    target.flush().map_err(|err| err.to_string())
}

pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let text =
        fs::read_to_string(carrier).map_err(|err| format!("Не удалось прочитать SVG: {err}"))?;
    let mut stack = Vec::new();
    let mut cursor = 0_usize;
    while let Some(found) = text[cursor..].find(METADATA_PREFIX) {
        let after_prefix = cursor + found + METADATA_PREFIX.len();
        let content_start = match text[after_prefix..].find('>') {
            Some(offset) => after_prefix + offset + 1,
            None => return Err("Повреждённый SVG: незакрытый тег метаданных.".into()),
        };
        let content_end = text[content_start..]
            .find(METADATA_CLOSE)
            .map(|index| index + content_start)
            .ok_or_else(|| "Повреждённый SVG: не найден конец метаданных.".to_owned())?;
        let decoded = STANDARD
            .decode(&text[content_start..content_end])
            .map_err(|err| format!("Повреждённая полезная нагрузка SVG: {err}"))?;
        stack.extend_from_slice(&decoded);
        cursor = content_end;
    }
    if stack.is_empty() {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    Ok(stack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip_through_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.svg");
        fs::write(
            &source,
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"1\" height=\"1\"/></svg>",
        )
        .unwrap();
        let output = dir.path().join("out.svg");
        let data = b"payload inside svg".to_vec();
        embed(&source, &data, data.len(), &output).unwrap();
        let text = fs::read_to_string(&output).unwrap();
        assert!(text.contains(METADATA_OPEN));
        assert!(text.trim_end().ends_with("</svg>"));
        assert_eq!(extract(&output).unwrap(), data);
    }

    #[test]
    fn second_embedding_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.svg");
        fs::write(&source, "<svg></svg>").unwrap();
        let output = dir.path().join("out.svg");
        embed(&source, b"one", 3, &output).unwrap();
        assert!(embed(&output, b"two", 3, &dir.path().join("again.svg")).is_err());
    }

    #[test]
    fn every_copy_is_stored_in_its_own_metadata_block() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.svg");
        fs::write(&source, "<svg></svg>").unwrap();
        let output = dir.path().join("out.svg");
        let stack = b"abcdefgh".repeat(4);
        embed(&source, &stack, 16, &output).unwrap();
        let text = fs::read_to_string(&output).unwrap();
        assert_eq!(text.matches("<metadata id=\"ufpl").count(), 2);
        assert_eq!(extract(&output).unwrap(), stack);
    }
}
