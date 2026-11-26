//! Нагрузка внутри пакета Office: часть `filepumper/payload.bin`.
//!
//! Часть объявлена в `[Content_Types].xml`, поэтому Word, Excel и PowerPoint
//! считают пакет корректным. Остальные части копируются без распаковки.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use zip::write::SimpleFileOptions as FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::util::Job;

pub const PART_NAME: &str = "filepumper/payload.bin";

/// Читает часть с нагрузкой из пакета.
pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(carrier).map_err(|err| err.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("Пакет Office повреждён: {err}"))?;
    let mut part = archive
        .by_name(PART_NAME)
        .map_err(|_| crate::payload::PAYLOAD_NOT_FOUND.to_owned())?;
    let mut bytes = Vec::new();
    part.read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// Встраивает нагрузку отдельной частью пакета.
pub fn embed(carrier: &Path, data: &[u8], output: &Path, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    let file = File::open(carrier).map_err(|err| err.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("Пакет Office повреждён: {err}"))?;
    let content_types = archive.index_for_name(crate::metadata::OPC_CONTENT_TYPES);
    let mut writer = ZipWriter::new(File::create(output).map_err(|err| err.to_string())?);
    let stored = FileOptions::default().compression_method(CompressionMethod::Stored);

    if let Some(index) = content_types {
        let mut entry = archive.by_index(index).map_err(|err| err.to_string())?;
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .map_err(|err| format!("Не удалось прочитать [Content_Types].xml: {err}"))?;
        drop(entry);
        writer
            .start_file(crate::metadata::OPC_CONTENT_TYPES, stored)
            .map_err(|err| err.to_string())?;
        writer
            .write_all(ensure_content_type(&xml).as_bytes())
            .map_err(|err| err.to_string())?;
    }

    for index in 0..archive.len() {
        if Some(index) == content_types {
            continue;
        }
        let entry = archive.by_index_raw(index).map_err(|err| err.to_string())?;
        // Старую нагрузку заменяем, а не дублируем.
        if entry.name() == PART_NAME {
            continue;
        }
        writer
            .raw_copy_file(entry)
            .map_err(|err| format!("Не удалось скопировать часть пакета: {err}"))?;
    }

    writer
        .start_file(PART_NAME, stored)
        .map_err(|err| err.to_string())?;
    writer.write_all(data).map_err(|err| err.to_string())?;
    writer
        .finish()
        .map_err(|err| format!("Не удалось завершить пакет: {err}"))?;
    Ok(())
}

/// Добавляет объявление типа для `.bin`-части, если его ещё нет.
fn ensure_content_type(xml: &str) -> String {
    if xml.contains("Extension=\"bin\"") || xml.contains("Extension='bin'") {
        return xml.to_owned();
    }
    let declaration = "<Default Extension=\"bin\" ContentType=\"application/octet-stream\"/>";
    match xml.rfind("</Types>") {
        Some(position) => {
            let mut result = String::with_capacity(xml.len() + declaration.len());
            result.push_str(&xml[..position]);
            result.push_str(declaration);
            result.push_str(&xml[position..]);
            result
        }
        None => xml.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{extract_payload, verify_file, Carrier};

    fn build_docx(path: &Path) {
        use zip::write::SimpleFileOptions as Options;
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = Options::default().compression_method(CompressionMethod::Deflated);
        let parts: [(&str, &[u8]); 3] = [
            (
                "[Content_Types].xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            ("_rels/.rels", br#"<Relationships/>"#),
            (
                "word/document.xml",
                br#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Hi</w:t></w:r></w:p></w:body></w:document>"#,
            ),
        ];
        for (name, data) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn payload_lives_in_a_declared_part_and_survives_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("report.docx");
        build_docx(&carrier);
        assert_eq!(
            crate::payload::detect_carrier(&carrier).unwrap(),
            Carrier::Opc
        );
        let output = dir.path().join("with_payload.docx");
        let outcome = crate::payload::embed_bytes(
            &carrier,
            b"secret inside docx",
            "secret.txt",
            &output,
            &crate::payload::ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();
        assert_eq!(outcome.carrier, Carrier::Opc);

        let (parsed, copies, kind) = extract_payload(&output, None).unwrap();
        assert_eq!(kind, Carrier::Opc);
        assert_eq!(copies, 1);
        assert_eq!(parsed.payload, b"secret inside docx");
        assert_eq!(parsed.name, "secret.txt");

        let verified = verify_file(&output, None).unwrap();
        assert_eq!(verified.name, "secret.txt");

        // Пакет остаётся ZIP с объявленным типом части.
        let file = File::open(&output).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(archive.index_for_name(PART_NAME).is_some());
        let mut types = String::new();
        archive
            .by_name("[Content_Types].xml")
            .unwrap()
            .read_to_string(&mut types)
            .unwrap();
        assert!(types.contains("Extension=\"bin\""), "{types}");
        assert!(archive.index_for_name("word/document.xml").is_some());
    }

    #[test]
    fn part_is_replaced_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("report.docx");
        build_docx(&carrier);
        let first = dir.path().join("first.docx");
        crate::payload::embed_bytes(
            &carrier,
            b"one",
            "one.txt",
            &first,
            &crate::payload::ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();
        let second = dir.path().join("second.docx");
        crate::payload::embed_bytes(
            &first,
            b"two",
            "two.txt",
            &second,
            &crate::payload::ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();
        let file = File::open(&second).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        assert_eq!(
            archive
                .file_names()
                .filter(|name| *name == PART_NAME)
                .count(),
            1
        );
        let (parsed, _, _) = extract_payload(&second, None).unwrap();
        assert_eq!(parsed.payload, b"two");
    }
}
