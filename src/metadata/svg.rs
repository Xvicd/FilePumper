//! SVG: элементы `<metadata>`, `<title>`, `<desc>` и комментарии.
//!
//! Метаданные FilePumper (`<metadata id="ufpl…">`) не трогаются: это полезная
//! нагрузка, а не сведения об авторе.

use std::fs;
use std::path::Path;

use crate::metadata::{CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

/// Префикс id метаданных FilePumper: их очистка сохраняет.
const PAYLOAD_ID: &[u8] = b"ufpl";

/// Похоже ли содержимое на самостоятельный SVG-файл.
pub(super) fn looks_like_svg(path: &Path, head: &[u8]) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    if !text.contains("<svg") {
        return false;
    }
    if extension == "svg" || extension == "svgz" {
        return true;
    }
    // HTML или XML с встроенным <svg> — не SVG-документ.
    if !extension.is_empty()
        && matches!(
            extension.as_str(),
            "html" | "htm" | "xhtml" | "xml" | "txt" | "js" | "css"
        )
    {
        return false;
    }
    head.windows(9)
        .any(|window| window.eq_ignore_ascii_case(b"</svg>"))
}

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let bytes = fs::read(input).map_err(|err| err.to_string())?;
    let total = bytes.len() as u64;
    let mut progress = Progress::new(total, job);
    if !options.text {
        output.write_all(&bytes).map_err(|err| err.to_string())?;
        progress.advance(total)?;
        return Ok(());
    }

    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            match bytes[index..].iter().position(|byte| *byte == b'<') {
                Some(offset) => {
                    result.extend_from_slice(&bytes[index..index + offset]);
                    progress.advance(offset as u64)?;
                    index += offset;
                }
                None => {
                    result.extend_from_slice(&bytes[index..]);
                    progress.advance((bytes.len() - index) as u64)?;
                    index = bytes.len();
                }
            }
            continue;
        }
        // Комментарий целиком.
        if bytes[index..].starts_with(b"<!--") {
            let end = find(&bytes[index + 4..], b"-->")
                .map(|offset| index + 4 + offset + 3)
                .ok_or_else(|| "Повреждённый SVG: комментарий не закрыт.".to_owned())?;
            report
                .removed
                .push(Removal::new("SVG комментарий", (end - index) as u64));
            progress.advance((end - index) as u64)?;
            index = end;
            continue;
        }
        // Закрывающие теги, DOCTYPE и инструкции обработки просто переносим:
        // содержимое удаляемых элементов вырезается целиком по закрывающему тегу.
        if matches!(bytes.get(index + 1), Some(b'/') | Some(b'!') | Some(b'?')) {
            let Some(end) = tag_end(&bytes, index) else {
                result.extend_from_slice(&bytes[index..]);
                index = bytes.len();
                continue;
            };
            result.extend_from_slice(&bytes[index..end + 1]);
            progress.advance((end + 1 - index) as u64)?;
            index = end + 1;
            continue;
        }
        let Some(open_end) = tag_end(&bytes, index) else {
            result.extend_from_slice(&bytes[index..]);
            index = bytes.len();
            continue;
        };
        let name = tag_name(&bytes[index..open_end]);
        if !is_metadata_element(&name) || is_payload(&bytes[index..open_end]) {
            let take = open_end + 1 - index;
            result.extend_from_slice(&bytes[index..open_end + 1]);
            progress.advance(take as u64)?;
            index = open_end + 1;
            continue;
        }
        let self_closing = bytes[open_end - 1] == b'/';
        let (end, label) = if self_closing {
            (open_end + 1, element_label(&name))
        } else {
            let close = close_tag(&bytes, open_end + 1, &name).ok_or_else(|| {
                format!(
                    "Повреждённый SVG: элемент <{}> не закрыт.",
                    String::from_utf8_lossy(&name)
                )
            })?;
            (close, element_label(&name))
        };
        report
            .removed
            .push(Removal::new(label, (end - index) as u64));
        progress.advance((end - index) as u64)?;
        index = end;
        // Съедаем перевод строки, оставшийся от удалённого элемента.
        while index < bytes.len() && matches!(bytes[index], b'\n' | b'\r' | b' ' | b'\t') {
            index += 1;
        }
    }
    output.write_all(&result).map_err(|err| err.to_string())?;
    progress.advance(0)?;
    Ok(())
}

fn is_metadata_element(name: &[u8]) -> bool {
    ["metadata", "title", "desc"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known.as_bytes()))
}

fn element_label(name: &[u8]) -> String {
    format!(
        "SVG <{}>",
        String::from_utf8_lossy(name).to_ascii_lowercase()
    )
}

/// Метаданные FilePumper: `<metadata id="ufpl">`, `ufpl2`, …
fn is_payload(open_tag: &[u8]) -> bool {
    let lower = open_tag.to_ascii_lowercase();
    for quote in *b"\"'" {
        let mut needle = b"id=".to_vec();
        needle.push(quote);
        needle.extend_from_slice(PAYLOAD_ID);
        if lower.windows(needle.len()).any(|window| window == needle) {
            return true;
        }
    }
    false
}

/// Имя элемента из открывающего тега: `<metadata id="x">` → `metadata`.
fn tag_name(tag: &[u8]) -> Vec<u8> {
    let mut start = 1;
    while start < tag.len() && matches!(tag[start], b'/' | b'!' | b'?') {
        start += 1;
    }
    let mut end = start;
    while end < tag.len() && !matches!(tag[end], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') {
        end += 1;
    }
    tag[start..end].to_vec()
}

/// Позиция `>` открывающего тега с учётом кавычек в атрибутах.
fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        match quote {
            Some(current) if *byte == current => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(*byte),
                b'>' => return Some(offset),
                _ => {}
            },
        }
    }
    None
}

/// Позиция сразу после `</name>`.
fn close_tag(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut index = from;
    while index + 2 < bytes.len() {
        if bytes[index] != b'<' || bytes[index + 1] != b'/' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        while end < bytes.len() && !matches!(bytes[end], b' ' | b'\t' | b'\r' | b'\n' | b'>') {
            end += 1;
        }
        if bytes[index + 2..end].eq_ignore_ascii_case(name) {
            let mut close = end;
            while close < bytes.len() && bytes[close] != b'>' {
                close += 1;
            }
            return (close < bytes.len()).then_some(close + 1);
        }
        index += 2;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    const SAMPLE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"",
        " xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n",
        "  <title>Мой рисунок</title>\n",
        "  <desc>Описание от автора</desc>\n",
        "  <metadata><dc:creator>Иван</dc:creator></metadata>\n",
        "  <metadata id=\"ufpl\">payload</metadata>\n",
        "  <!-- служебный комментарий -->\n",
        "  <rect x=\"0\" y=\"0\" width=\"10\" height=\"10\" fill=\"red\"/>\n",
        "  <text x=\"1\" y=\"8\" font-size=\"6\">Hi</text>\n",
        "</svg>\n",
    );

    #[test]
    fn author_metadata_is_removed_and_payload_kept() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("drawing.svg");
        fs::write(&input, SAMPLE).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Svg);

        let output = dir.path().join("clean.svg");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 4, "{:?}", report.removed);

        let text = fs::read_to_string(&output).unwrap();
        assert!(!text.contains("Мой рисунок"));
        assert!(!text.contains("Описание от автора"));
        assert!(!text.contains("Иван"));
        assert!(!text.contains("служебный комментарий"));
        assert!(text.contains("id=\"ufpl\""), "нагрузка должна остаться");
        assert!(text.contains("<rect"));
        assert!(text.contains("</svg>"));
    }

    #[test]
    fn attributes_with_angle_brackets_do_not_break_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><title>a</title>\
                   <rect fill=\"url(#a>b)\" width=\"2\" height=\"2\"/></svg>";
        let input = dir.path().join("tricky.svg");
        fs::write(&input, svg).unwrap();
        let output = dir.path().join("clean.svg");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 1);
        let text = fs::read_to_string(&output).unwrap();
        assert!(
            text.contains("url(#a>b)"),
            "атрибут не должен пострадать: {text}"
        );
    }
}
