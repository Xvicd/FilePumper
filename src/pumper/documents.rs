//! Документы «с нуля»: PDF, DOCX и XLSX ровно нужного размера.
//!
//! Внутри — маленький настоящий документ и заполнитель из криптослучайных
//! байтов: PDF-поток, на который никто не ссылается, или часть пакета OPC.
//! Поэтому файл открывается программой, а архивом не сжимается.

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use zip::write::SimpleFileOptions as FileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::pumper::GeneratedKind;
use crate::util::{random_bytes, Job, IO_CHUNK};

/// Каркас документа без заполнителя.
pub fn skeleton(kind: GeneratedKind) -> Result<Vec<u8>, String> {
    match kind {
        GeneratedKind::Pdf => pdf_with_padding(0),
        GeneratedKind::Docx | GeneratedKind::Xlsx => opc_package(kind, 0),
        other => Err(format!("«{}» — не документ.", other.title())),
    }
}

/// Минимальный размер документа: меньше каркаса не выйдет.
pub fn min_size(kind: GeneratedKind) -> Result<u64, String> {
    Ok(skeleton(kind)?.len() as u64)
}

/// Создаёт документ ровно на `target` байт.
pub fn build(
    kind: GeneratedKind,
    output: &Path,
    target: u64,
    job: &Job<'_>,
) -> Result<u64, String> {
    job.check()?;
    let min = min_size(kind)?;
    if target < min {
        return Err(format!(
            "Для файла типа «{}» нужно минимум {min} байт.",
            kind.title()
        ));
    }
    let bytes = match kind {
        GeneratedKind::Pdf => pdf_to_size(target)?,
        GeneratedKind::Docx | GeneratedKind::Xlsx => opc_to_size(kind, target)?,
        other => return Err(format!("«{}» — не документ.", other.title())),
    };
    job.report(90.0, "Запись документа...");
    if bytes.len() as u64 != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {}.",
            bytes.len()
        ));
    }
    fs::write(output, &bytes).map_err(|err| err.to_string())?;
    job.report(100.0, "Готово.");
    Ok(target)
}

// ---------------------------------------------------------------- PDF

/// Текст на странице PDF: длина потока считается по этим байтам.
const PDF_CONTENT: &[u8] = b"BT /F1 24 Tf 72 770 Td (FilePumper) Tj ET";

/// PDF ровно на `target` байт: размер подбирается длиной заполнителя.
fn pdf_to_size(target: u64) -> Result<Vec<u8>, String> {
    let mut padding = target.saturating_sub(pdf_with_padding(0)?.len() as u64);
    for _ in 0..8 {
        let bytes = pdf_with_padding(padding)?;
        if bytes.len() as u64 == target {
            return Ok(bytes);
        }
        let difference = target as i64 - bytes.len() as i64;
        let next = padding as i64 + difference;
        if next < 0 {
            return Err(format!("Для PDF нужно минимум {target} байт."));
        }
        padding = next as u64;
    }
    Err("Не удалось попасть точно в нужный размер PDF.".to_owned())
}

/// Валидный PDF с заполнителем в отдельном потоке, на который никто не ссылается.
fn pdf_with_padding(padding: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut offsets = Vec::with_capacity(6);
    out.extend_from_slice(b"%PDF-1.4\n");

    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
          /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n",
    );

    offsets.push(out.len());
    out.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", PDF_CONTENT.len()).as_bytes(),
    );
    out.extend_from_slice(PDF_CONTENT);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    offsets.push(out.len());
    out.extend_from_slice(format!("6 0 obj\n<< /Length {padding} >>\nstream\n").as_bytes());
    let mut left = padding;
    while left > 0 {
        let size = left.min(IO_CHUNK as u64) as usize;
        let bytes = random_bytes(size)?;
        out.extend_from_slice(&bytes);
        left -= size as u64;
    }
    out.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    Ok(out)
}

// ---------------------------------------------------------------- OPC

/// Байты `[Content_Types].xml` для пакета нужного типа.
fn content_types(kind: GeneratedKind) -> &'static str {
    match kind {
        GeneratedKind::Docx => concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
            "<Default Extension=\"rels\" ContentType=\"application/",
            "vnd.openxmlformats-package.relationships+xml\"/>",
            "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
            "<Default Extension=\"fppad\" ContentType=\"application/octet-stream\"/>",
            "<Override PartName=\"/word/document.xml\" ContentType=\"application/",
            "vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
            "</Types>"
        ),
        _ => concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
            "<Default Extension=\"rels\" ContentType=\"application/",
            "vnd.openxmlformats-package.relationships+xml\"/>",
            "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
            "<Default Extension=\"fppad\" ContentType=\"application/octet-stream\"/>",
            "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/",
            "vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>",
            "<Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/",
            "vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
            "</Types>"
        ),
    }
}

const ROOT_RELS_DOCX: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/",
    "2006/relationships/officeDocument\" Target=\"word/document.xml\"/>",
    "</Relationships>"
);

const ROOT_RELS_XLSX: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/",
    "2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>",
    "</Relationships>"
);

const DOCUMENT_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:body><w:p><w:r><w:t>FilePumper</w:t></w:r></w:p>",
    "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:body></w:document>"
);

const WORKBOOK_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"",
    " xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">",
    "<sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
);

const WORKBOOK_RELS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/",
    "2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>",
    "</Relationships>"
);

const SHEET_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
    "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    "<sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>FilePumper</t></is></c>",
    "</row></sheetData></worksheet>"
);

fn opc_parts(kind: GeneratedKind) -> Vec<(&'static str, &'static str)> {
    match kind {
        GeneratedKind::Docx => vec![
            ("_rels/.rels", ROOT_RELS_DOCX),
            ("word/document.xml", DOCUMENT_XML),
        ],
        _ => vec![
            ("_rels/.rels", ROOT_RELS_XLSX),
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", SHEET_XML),
        ],
    }
}

/// Пакет OPC с частью-заполнителем заданной длины.
fn opc_package(kind: GeneratedKind, padding: u64) -> Result<Vec<u8>, String> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut buffer);
        let deflated = FileOptions::default().compression_method(CompressionMethod::Deflated);
        let stored = FileOptions::default().compression_method(CompressionMethod::Stored);

        writer
            .start_file("[Content_Types].xml", deflated)
            .map_err(|err| err.to_string())?;
        writer
            .write_all(content_types(kind).as_bytes())
            .map_err(|err| err.to_string())?;
        for (name, data) in opc_parts(kind) {
            writer
                .start_file(name, deflated)
                .map_err(|err| err.to_string())?;
            writer
                .write_all(data.as_bytes())
                .map_err(|err| err.to_string())?;
        }
        writer
            .start_file("filepumper/padding.fppad", stored)
            .map_err(|err| err.to_string())?;
        let mut left = padding;
        while left > 0 {
            let size = left.min(IO_CHUNK as u64) as usize;
            let bytes = random_bytes(size)?;
            writer.write_all(&bytes).map_err(|err| err.to_string())?;
            left -= size as u64;
        }
        writer.finish().map_err(|err| err.to_string())?;
    }
    Ok(buffer.into_inner())
}

/// Пакет OPC ровно на `target` байт: заполнитель — несжатая часть в конце.
fn opc_to_size(kind: GeneratedKind, target: u64) -> Result<Vec<u8>, String> {
    let empty = opc_package(kind, 0)?;
    if empty.len() as u64 > target {
        return Err(format!(
            "Для этого типа нужно минимум {} байт.",
            empty.len()
        ));
    }
    let padding = target - empty.len() as u64;
    let bytes = opc_package(kind, padding)?;
    if bytes.len() as u64 != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {}.",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pumper::GeneratedKind;

    #[test]
    fn documents_hit_the_exact_size() {
        let dir = tempfile::tempdir().unwrap();
        for kind in [GeneratedKind::Pdf, GeneratedKind::Docx, GeneratedKind::Xlsx] {
            let output = dir.path().join(format!("made.{}", kind.extension()));
            let target = 64 * 1024;
            build(kind, &output, target, &Job::quiet()).unwrap();
            assert_eq!(fs::metadata(&output).unwrap().len(), target);
        }
    }

    #[test]
    fn generated_documents_stay_readable() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("made.pdf");
        build(GeneratedKind::Pdf, &pdf, 32 * 1024, &Job::quiet()).unwrap();
        let bytes = fs::read(&pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        let docx = dir.path().join("made.docx");
        build(GeneratedKind::Docx, &docx, 32 * 1024, &Job::quiet()).unwrap();
        let archive = zip::ZipArchive::new(fs::File::open(&docx).unwrap()).unwrap();
        assert!(archive.index_for_name("word/document.xml").is_some());
        assert!(archive.index_for_name("filepumper/padding.fppad").is_some());

        let xlsx = dir.path().join("made.xlsx");
        build(GeneratedKind::Xlsx, &xlsx, 32 * 1024, &Job::quiet()).unwrap();
        let archive = zip::ZipArchive::new(fs::File::open(&xlsx).unwrap()).unwrap();
        assert!(archive.index_for_name("xl/workbook.xml").is_some());
        assert!(archive.index_for_name("xl/worksheets/sheet1.xml").is_some());
    }

    #[test]
    fn too_small_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("tiny.pdf");
        let error = build(GeneratedKind::Pdf, &output, 8, &Job::quiet()).unwrap_err();
        assert!(error.contains("минимум"), "{error}");
        assert!(!output.exists());
    }
}
