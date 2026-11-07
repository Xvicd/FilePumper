//! Определение типа файла-носителя.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
    Raw,
}

impl Carrier {
    pub fn label(self) -> &'static str {
        match self {
            Carrier::Png => "PNG",
            Carrier::Jpeg => "JPEG",
            Carrier::Gif => "GIF",
            Carrier::Webp => "WebP",
            Carrier::Svg => "SVG",
            Carrier::Raw => "обычный файл",
        }
    }

    /// Где окажется нагрузка — показывается в интерфейсе.
    pub fn placement(self) -> &'static str {
        match self {
            Carrier::Png => "приватные чанки ufPl перед IEND",
            Carrier::Jpeg => "сегменты APP15 сразу после SOI",
            Carrier::Gif => "блок Comment Extension перед трейлером",
            Carrier::Webp => "RIFF-чанк UFP1 с пересчётом размера",
            Carrier::Svg => "элемент <metadata id=\"ufpl\"> перед </svg>",
            Carrier::Raw => "контейнер в конце файла с концевым маркером",
        }
    }

    /// Насколько безопасно файл переживёт загрузку на сервер.
    pub fn survives_upload(self) -> &'static str {
        match self {
            Carrier::Png | Carrier::Jpeg | Carrier::Webp => {
                "высокая: данные внутри структуры изображения, хвост не нужен"
            }
            Carrier::Gif => "высокая: комментарий читается декодером как часть формата",
            Carrier::Svg => "средняя: данные в тексте, чувствительно к минификации",
            Carrier::Raw => "низкая: данные в хвосте файла, сервер может их отрезать",
        }
    }
}

fn read_head(path: &Path, size: usize) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut head = vec![0_u8; size];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) => return Err(err.to_string()),
        }
    }
    head.truncate(filled);
    Ok(head)
}

fn read_tail(path: &Path, size: u64) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let total = file.metadata().map_err(|err| err.to_string())?.len();
    let start = total.saturating_sub(size);
    file.seek(SeekFrom::Start(start))
        .map_err(|err| err.to_string())?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).map_err(|err| err.to_string())?;
    Ok(tail)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .is_some_and(|value| extensions.contains(&value.as_str()))
}

/// Тип определяется по сигнатуре, расширение используется как уточнение
/// для текстовых SVG, где сигнатуры нет.
pub fn detect_carrier(path: &Path) -> Result<Carrier, String> {
    let head = read_head(path, 4096)?;
    if head.starts_with(&PNG_SIGNATURE) {
        return Ok(Carrier::Png);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(Carrier::Jpeg);
    }
    if head.starts_with(b"GIF8") {
        return Ok(Carrier::Gif);
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Ok(Carrier::Webp);
    }
    if has_extension(path, &["svg", "svgz"]) {
        return Ok(Carrier::Svg);
    }
    let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    if !head_text.contains("<svg") {
        return Ok(Carrier::Raw);
    }
    // HTML/XML с встроенным <svg> — не SVG-носитель: там нельзя вставлять метаданные.
    if has_extension(path, &["html", "htm", "xhtml", "xml", "txt", "js", "css"]) {
        return Ok(Carrier::Raw);
    }
    let tail = read_tail(path, 4096)?;
    let tail_text = String::from_utf8_lossy(&tail).to_ascii_lowercase();
    if tail_text.contains("</svg>") {
        Ok(Carrier::Svg)
    } else {
        Ok(Carrier::Raw)
    }
}
