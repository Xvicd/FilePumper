//! PDF: свойства документа (`/Info`) и XMP-метаданные (`/Metadata`).
//!
//! PDF правится без пересборки: из словаря-трейлера убираются ссылки на
//! свойства и XMP, а сам объект `/Info` заменяется пустым словарём той же
//! длины, поэтому смещения объектов и `startxref` остаются верными.

use std::fs;
use std::path::Path;

use crate::metadata::{CleanOptions, Progress, Removal, Report, SeekWrite};
use crate::util::Job;

/// Ключи трейлера, которые удаляются.
const KEYS: [&str; 2] = ["Info", "Metadata"];

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let data = fs::read(input).map_err(|err| err.to_string())?;
    let total = data.len() as u64;
    let mut progress = Progress::new(total, job);
    if data.len() < 32 || !data.starts_with(b"%PDF") {
        return Err("Файл не является PDF.".to_owned());
    }
    let mut result = data.clone();
    if options.text {
        // Правки применяются от конца к началу, чтобы смещения не съезжали.
        let mut edits: Vec<Edit> = Vec::new();
        if let Some((dict_start, dict_end)) = trailer_dict(&data)? {
            let (stripped, removed) = strip_keys(&data[dict_start + 2..dict_end], &KEYS);
            for (key, bytes, value) in &removed {
                let label = if key == "Info" {
                    "PDF /Info (свойства документа)".to_owned()
                } else {
                    "PDF /Metadata (XMP-метаданные)".to_owned()
                };
                report.removed.push(Removal::new(label, *bytes));
                // Значения /Info прячем в самом объекте, а не только ссылку.
                if key == "Info" {
                    if let Some(number) = first_number(value) {
                        if let Some((start, end)) = find_object_body(&data, number) {
                            let replacement = blank_object(end - start);
                            edits.push(Edit {
                                start,
                                end,
                                bytes: replacement,
                            });
                            report.removed.push(Removal::new(
                                format!("PDF объект /Info {number}: значения"),
                                (end - start) as u64,
                            ));
                        }
                    }
                }
            }
            if stripped.len() != dict_end - dict_start - 2 {
                edits.push(Edit {
                    start: dict_start + 2,
                    end: dict_end,
                    bytes: stripped,
                });
            }
        }
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
        for edit in edits {
            if edit.end <= result.len() {
                result.splice(edit.start..edit.end, edit.bytes);
            }
        }
    }
    output.write_all(&result).map_err(|err| err.to_string())?;
    progress.advance(total)?;
    Ok(())
}

struct Edit {
    start: usize,
    end: usize,
    bytes: Vec<u8>,
}

/// Пустой словарь, дополненный пробелами до прежней длины.
fn blank_object(length: usize) -> Vec<u8> {
    let mut bytes = b"<<>>".to_vec();
    if bytes.len() < length {
        bytes.resize(length, b' ');
    }
    bytes
}

/// Первое число из значения вида `3 0 R`.
fn first_number(value: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(value);
    text.split_whitespace().next()?.parse().ok()
}

/// Границы словаря-трейлера: классического (`trailer << … >>`) или
/// словаря xref-потока.
fn trailer_dict(data: &[u8]) -> Result<Option<(usize, usize)>, String> {
    let Some(startxref) = rfind(data, b"startxref") else {
        return Ok(None);
    };
    let mut index = startxref + b"startxref".len();
    while index < data.len() && data[index].is_ascii_whitespace() {
        index += 1;
    }
    let mut offset = 0_usize;
    let mut digits = 0;
    while index < data.len() && data[index].is_ascii_digit() {
        offset = offset * 10 + usize::from(data[index] - b'0');
        index += 1;
        digits += 1;
    }
    if digits == 0 || offset >= data.len() {
        return Ok(None);
    }
    let anchor = if data[offset..].starts_with(b"xref") {
        match find(&data[offset..], b"trailer") {
            Some(position) => offset + position,
            None => return Ok(None),
        }
    } else {
        offset
    };
    let Some(dict_start) = find(&data[anchor..], b"<<").map(|position| anchor + position) else {
        return Ok(None);
    };
    let Some(dict_end) = dict_end(data, dict_start) else {
        return Ok(None);
    };
    Ok(Some((dict_start, dict_end + 2)))
}

/// Индекс первого байта закрывающих `>>`.
fn dict_end(data: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let mut index = start;
    while index + 1 < data.len() {
        match (data[index], data[index + 1]) {
            (b'<', b'<') => {
                depth += 1;
                index += 2;
            }
            (b'>', b'>') => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                index += 2;
            }
            (b'(', _) => index = skip_string(data, index)?,
            (b'%', _) => {
                while index < data.len() && data[index] != b'\n' {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// Индекс сразу после закрывающей скобки строки с учётом экранирования.
fn skip_string(data: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let mut index = start;
    while index < data.len() {
        match data[index] {
            b'\\' => index += 2,
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Name,
    Value,
    Space,
}

struct Piece {
    start: usize,
    end: usize,
    token: Token,
}

/// Разбивает содержимое словаря на имена, значения и промежутки.
fn tokenize(dict: &[u8]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut index = 0;
    while index < dict.len() {
        let start = index;
        let token = match dict[index] {
            byte if byte.is_ascii_whitespace() => {
                while index < dict.len() && dict[index].is_ascii_whitespace() {
                    index += 1;
                }
                Token::Space
            }
            b'%' => {
                while index < dict.len() && dict[index] != b'\n' {
                    index += 1;
                }
                Token::Space
            }
            b'/' => {
                index += 1;
                while index < dict.len() && !is_delimiter(dict[index]) {
                    index += 1;
                }
                Token::Name
            }
            b'(' => {
                index = skip_string(dict, index).unwrap_or(dict.len());
                Token::Value
            }
            b'<' if dict.get(index + 1) == Some(&b'<') => {
                index = dict_end(dict, index)
                    .map(|position| position + 2)
                    .unwrap_or(dict.len());
                Token::Value
            }
            b'<' => {
                index += 1;
                while index < dict.len() && dict[index] != b'>' {
                    index += 1;
                }
                index = (index + 1).min(dict.len());
                Token::Value
            }
            b'[' => {
                index = array_end(dict, index).unwrap_or(dict.len());
                Token::Value
            }
            _ => {
                while index < dict.len() && !is_delimiter(dict[index]) {
                    index += 1;
                }
                if index == start {
                    index += 1;
                }
                Token::Value
            }
        };
        pieces.push(Piece {
            start,
            end: index,
            token,
        });
    }
    pieces
}

fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(byte, b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'%')
}

fn array_end(data: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let mut index = start;
    while index < data.len() {
        match data[index] {
            b'(' => index = skip_string(data, index)?,
            b'[' => {
                depth += 1;
                index += 1;
            }
            b']' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// Убранная из словаря запись: ключ, сколько байт заняла и её значение.
type RemovedKey = (String, u64, Vec<u8>);

/// Убирает из словаря записи с указанными ключами.
///
/// Возвращает новый словарь и список удалённого: ключ, сколько байт занял,
/// неразобранное значение.
fn strip_keys(dict: &[u8], keys: &[&str]) -> (Vec<u8>, Vec<RemovedKey>) {
    let pieces = tokenize(dict);
    let mut out = Vec::with_capacity(dict.len());
    let mut removed = Vec::new();
    let mut index = 0;
    while index < pieces.len() {
        let piece = &pieces[index];
        if piece.token == Token::Name {
            let name = String::from_utf8_lossy(&dict[piece.start + 1..piece.end]).into_owned();
            if keys.iter().any(|key| *key == name) {
                let mut end = piece.end;
                let mut value = Vec::new();
                let mut next = index + 1;
                while next < pieces.len() {
                    match pieces[next].token {
                        Token::Name => break,
                        Token::Space => {
                            end = pieces[next].end;
                            value.extend_from_slice(&dict[pieces[next].start..pieces[next].end]);
                            next += 1;
                        }
                        Token::Value => {
                            value.extend_from_slice(&dict[pieces[next].start..pieces[next].end]);
                            end = pieces[next].end;
                            next += 1;
                        }
                    }
                }
                removed.push((name, (end - piece.start) as u64, value));
                index = next;
                continue;
            }
        }
        out.extend_from_slice(&dict[piece.start..piece.end]);
        index += 1;
    }
    (out, removed)
}

/// Границы тела объекта `N G obj … endobj`: от `obj` до `endobj`.
fn find_object_body(data: &[u8], number: u32) -> Option<(usize, usize)> {
    let mut search = 0;
    while let Some(position) = find(&data[search..], b"obj") {
        let obj_at = search + position;
        // Перед «obj» должно быть число и номер объекта.
        let mut index = obj_at;
        let mut numbers = Vec::new();
        while numbers.len() < 2 {
            while index > 0 && data[index - 1].is_ascii_whitespace() {
                index -= 1;
            }
            let end = index;
            while index > 0 && data[index - 1].is_ascii_digit() {
                index -= 1;
            }
            if end == index {
                break;
            }
            numbers.push(
                String::from_utf8_lossy(&data[index..end])
                    .parse::<u32>()
                    .ok()?,
            );
        }
        if numbers.len() == 2 && numbers[1] == number {
            let body_start = obj_at + 3;
            let body_end = find(&data[body_start..], b"endobj")
                .map(|offset| body_start + offset)
                .unwrap_or(data.len());
            if body_end > body_start {
                return Some((body_start, body_end));
            }
        }
        search = obj_at + 3;
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

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};

    /// Минимальный PDF с /Info и XMP: смещения считаются по месту.
    fn sample_pdf() -> Vec<u8> {
        let mut objects: Vec<(u32, Vec<u8>)> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec()),
            (
                3,
                b"<< /Author (Ivan Petrov) /Creator (Word) /Title (Report) >>".to_vec(),
            ),
            (
                4,
                b"<< /Type /Metadata /Subtype /XML /Length 22 >>\nstream\n<x:xmpmeta>secret</x:xmpmeta>\nendstream"
                    .to_vec(),
            ),
        ];
        objects.sort_by_key(|(number, _)| *number);
        let mut body = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (number, content) in &objects {
            offsets.push(body.len());
            body.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            body.extend_from_slice(content);
            body.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = body.len();
        body.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        body.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            body.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        body.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R /Metadata 4 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        body
    }

    #[test]
    fn info_and_xmp_references_are_removed_and_values_blanked() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("doc.pdf");
        fs::write(&input, sample_pdf()).unwrap();
        assert_eq!(detect(&input).unwrap(), Kind::Pdf);

        let output = dir.path().join("clean.pdf");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert!(report.removed.len() >= 2, "{:?}", report.removed);

        let text = String::from_utf8_lossy(&fs::read(&output).unwrap()).into_owned();
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("startxref"));
        assert!(text.contains("xref"));
        // Ссылок больше нет, а сами значения в объекте вычищены.
        assert!(!text.contains("/Info 3 0 R"));
        assert!(!text.contains("/Metadata 4 0 R"));
        assert!(!text.contains("Ivan Petrov"));
        assert!(!text.contains("(Word)"));
        assert!(!text.contains("(Report)"));
        // Содержимое документа на месте.
        assert!(text.contains("/Type /Catalog"));
        // Размер изменился не сильно: правится только трейлер и объект свойств.
        let cleaned = fs::metadata(&output).unwrap().len();
        let original = fs::metadata(&input).unwrap().len();
        assert!(cleaned < original);
        assert!(original - cleaned < 200, "разница {}", original - cleaned);
    }

    #[test]
    fn file_without_metadata_is_copied_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let mut pdf = sample_pdf();
        // Убираем из трейлера ссылки — метаданных нет.
        let text = String::from_utf8_lossy(&pdf).replace(" /Info 3 0 R /Metadata 4 0 R", "");
        pdf = text.into_bytes();
        let input = dir.path().join("plain.pdf");
        fs::write(&input, &pdf).unwrap();
        let output = dir.path().join("clean.pdf");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert!(report.is_empty(), "{:?}", report.removed);
        assert_eq!(fs::read(&output).unwrap(), pdf);
    }
}
