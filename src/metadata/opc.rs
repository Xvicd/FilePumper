//! Пакеты OPC (`docx`, `xlsx`, `pptx`): свойства документа и превью.

use std::fs::File;
use std::path::Path;

use zip::{ZipArchive, ZipWriter};

use crate::metadata::{CleanOptions, Removal, Report, SeekWrite};
use crate::util::Job;

/// Служебная часть OPC со списком типов содержимого.
pub const OPC_CONTENT_TYPES: &str = "[Content_Types].xml";

const CORE: &str = "docProps/core.xml";
const APP: &str = "docProps/app.xml";
const CUSTOM: &str = "docProps/custom.xml";
const THUMBNAIL_PREFIX: &str = "docProps/thumbnail";

/// Пустые свойства: структура и пространства имён на месте, значений нет.
const BLANK_CORE: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<cp:coreProperties",
    " xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\"",
    " xmlns:dc=\"http://purl.org/dc/elements/1.1/\"",
    " xmlns:dcterms=\"http://purl.org/dc/terms/\"",
    " xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\"",
    " xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"/>"
);
const BLANK_APP: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Properties",
    " xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"",
    " xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"/>"
);
const BLANK_CUSTOM: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Properties",
    " xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\"",
    " xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"/>"
);

/// Прозрачная точка 1×1 вместо превью документа.
const BLANK_THUMBNAIL: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0x00, 0x00, 0x00, 0x02,
    0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE, 0x42,
    0x60, 0x82,
];

/// Открывается ли файл как пакет OPC.
pub fn is_package(path: &Path) -> bool {
    match File::open(path) {
        Ok(file) => match ZipArchive::new(file) {
            Ok(archive) => archive.index_for_name(OPC_CONTENT_TYPES).is_some(),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

pub(super) fn clean(
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    let file = File::open(input).map_err(|err| err.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("Не удалось открыть пакет Office: {err}"))?;
    let content_types = archive.index_for_name(OPC_CONTENT_TYPES);
    let mut writer = ZipWriter::new(output);

    // Типы содержимого обязаны идти первыми.
    if let Some(index) = content_types {
        let entry = archive.by_index_raw(index).map_err(|err| err.to_string())?;
        writer
            .raw_copy_file(entry)
            .map_err(|err| format!("Не удалось скопировать «{OPC_CONTENT_TYPES}»: {err}"))?;
    }

    for index in 0..archive.len() {
        job.check()?;
        if Some(index) == content_types {
            continue;
        }
        let entry = archive.by_index_raw(index).map_err(|err| err.to_string())?;
        let name = entry.name().to_owned();
        let replacement: Option<(&str, &[u8])> = match name.as_str() {
            CORE if options.text => {
                Some(("свойства документа (автор, даты)", BLANK_CORE.as_bytes()))
            }
            APP if options.text => Some(("приложение и компания", BLANK_APP.as_bytes())),
            CUSTOM if options.text => Some(("пользовательские свойства", BLANK_CUSTOM.as_bytes())),
            _ if options.profiles && name.starts_with(THUMBNAIL_PREFIX) => {
                Some(("превью документа", BLANK_THUMBNAIL))
            }
            _ => None,
        };
        match replacement {
            Some((what, bytes)) => {
                let options = entry.options();
                let original = entry.size();
                drop(entry);
                writer
                    .start_file(name.as_str(), options)
                    .map_err(|err| format!("Не удалось переписать «{name}»: {err}"))?;
                std::io::Write::write_all(&mut writer, bytes).map_err(|err| err.to_string())?;
                report
                    .removed
                    .push(Removal::new(format!("{name}: {what}"), original));
            }
            None => {
                writer
                    .raw_copy_file(entry)
                    .map_err(|err| format!("Не удалось скопировать «{name}»: {err}"))?;
            }
        }
    }
    writer
        .finish()
        .map_err(|err| format!("Не удалось завершить пакет: {err}"))?;
    Ok(())
}

/// Прочитать часть пакета целиком — нужно тестам.
#[cfg(test)]
pub fn read_part(path: &Path, part: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = File::open(path).map_err(|err| err.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("Не удалось открыть пакет Office: {err}"))?;
    let mut entry = archive
        .by_name(part)
        .map_err(|_| format!("В пакете нет части «{part}»."))?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// Список частей пакета — нужно тестам.
#[cfg(test)]
pub fn parts(path: &Path) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let archive =
        ZipArchive::new(file).map_err(|err| format!("Не удалось открыть пакет Office: {err}"))?;
    Ok(archive.file_names().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{clean, detect, Kind};
    use std::io::Write;
    use zip::write::SimpleFileOptions as FileOptions;
    use zip::CompressionMethod;

    const CONTENT_TYPES: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/",
        "vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>",
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/",
        "vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
        "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/",
        "vnd.openxmlformats-package.core-properties+xml\"/>",
        "<Override PartName=\"/docProps/app.xml\" ContentType=\"application/",
        "vnd.openxmlformats-officedocument.extended-properties+xml\"/>",
        "<Override PartName=\"/docProps/custom.xml\" ContentType=\"application/",
        "vnd.openxmlformats-officedocument.custom-properties+xml\"/>",
        "</Types>"
    );

    fn build_docx(path: &Path) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
        let parts: Vec<(&str, &[u8])> = vec![
            (OPC_CONTENT_TYPES, CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", b"<Relationships/>"),
            ("word/document.xml", b"<w:document xmlns:w=\"w\"/>"),
            (
                "docProps/core.xml",
                concat!(
                    "<?xml version=\"1.0\"?><cp:coreProperties",
                    " xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\"",
                    " xmlns:dc=\"http://purl.org/dc/elements/1.1/\">",
                    "<dc:creator>Иван Петров</dc:creator>",
                    "<cp:lastModifiedBy>Иван</cp:lastModifiedBy>",
                    "<dc:title>Отчёт</dc:title>",
                    "</cp:coreProperties>"
                )
                .as_bytes(),
            ),
            (
                "docProps/app.xml",
                "<Properties><Application>Microsoft Office Word</Application><Company>ООО Ромашка</Company></Properties>"
                    .as_bytes(),
            ),
            (
                "docProps/custom.xml",
                "<Properties><property name=\"Manager\"><vt:lpwstr>Иван</vt:lpwstr></property></Properties>"
                    .as_bytes(),
            ),
            ("docProps/thumbnail.jpeg", b"fake jpeg preview"),
        ];
        for (name, data) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn document_properties_are_blanked_and_package_stays_valid() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("report.docx");
        build_docx(&input);
        assert_eq!(detect(&input).unwrap(), Kind::Opc);
        assert!(is_package(&input));

        let output = dir.path().join("clean.docx");
        let report = clean(&input, &output, CleanOptions::default(), &Job::quiet()).unwrap();
        assert_eq!(report.removed.len(), 3, "{:?}", report.removed);

        let core = read_part(&output, CORE).unwrap();
        let core = String::from_utf8(core).unwrap();
        assert!(!core.contains("Иван"));
        assert!(!core.contains("Отчёт"));
        assert!(core.contains("coreProperties"));

        let app = read_part(&output, APP).unwrap();
        assert!(!String::from_utf8_lossy(&app).contains("Ромашка"));
        let custom = read_part(&output, CUSTOM).unwrap();
        assert!(!String::from_utf8_lossy(&custom).contains("Manager"));

        // Основная часть и превью (по умолчанию) не тронуты.
        let document = read_part(&output, "word/document.xml").unwrap();
        assert!(!document.is_empty());
        let thumbnail = read_part(&output, "docProps/thumbnail.jpeg").unwrap();
        assert_eq!(thumbnail, b"fake jpeg preview");
        // Пакет читается как архив, порядок частей сохранён.
        let names = parts(&output).unwrap();
        assert_eq!(names.first().map(String::as_str), Some(OPC_CONTENT_TYPES));
    }

    #[test]
    fn thumbnail_is_replaced_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("report.docx");
        build_docx(&input);
        let output = dir.path().join("clean.docx");
        let report = clean(
            &input,
            &output,
            CleanOptions {
                text: false,
                profiles: true,
            },
            &Job::quiet(),
        )
        .unwrap();
        assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
        let thumbnail = read_part(&output, "docProps/thumbnail.jpeg").unwrap();
        assert!(thumbnail.starts_with(&crate::payload::carrier::PNG_SIGNATURE));
        // Свойства остались на месте.
        let core = read_part(&output, CORE).unwrap();
        assert!(String::from_utf8_lossy(&core).contains("Иван"));
    }
}
