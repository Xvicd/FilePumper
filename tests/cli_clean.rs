//! Проверка команды `clean`: метаданные удаляются, файлы остаются рабочими.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions as FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|value| (*value).to_owned()).collect()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn cli(list: &[&str]) -> i32 {
    file_pumper::cli::run(&args(list))
}

/// PNG с текстовым чанком: так выглядит файл, прошедший через редактор.
fn png_with_text(dir: &Path) -> PathBuf {
    let path = dir.join("shot.png");
    image::RgbImage::from_fn(4, 4, |x, y| image::Rgb([x as u8, y as u8, 10]))
        .save(&path)
        .unwrap();
    let mut png = fs::read(&path).unwrap();
    let data: &[u8] = b"Software\0FilePumper";
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(b"tEXt");
    hasher.update(data);
    chunk.extend_from_slice(&hasher.finalize().to_be_bytes());
    // Сигнатура (8) + IHDR (12 + 13): вставляем сразу после IHDR.
    let insert_at = 8 + 12 + 13;
    png.splice(insert_at..insert_at, chunk);
    fs::write(&path, png).unwrap();
    path
}

/// Минимальный пакет OPC со свойствами документа.
fn docx_with_properties(dir: &Path) -> PathBuf {
    let path = dir.join("report.docx");
    let content_types = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/",
        "vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/",
        "vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
        "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/",
        "vnd.openxmlformats-package.core-properties+xml\"/>",
        "</Types>"
    );
    let core = concat!(
        "<?xml version=\"1.0\"?><cp:coreProperties",
        " xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\"",
        " xmlns:dc=\"http://purl.org/dc/elements/1.1/\">",
        "<dc:creator>Иван Петров</dc:creator><dc:title>Отчёт</dc:title>",
        "</cp:coreProperties>"
    );
    let mut writer = ZipWriter::new(fs::File::create(&path).unwrap());
    let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in [
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", "<Relationships/>"),
        ("word/document.xml", "<w:document xmlns:w=\"w\"/>"),
        ("docProps/core.xml", core),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(data.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
    path
}

/// Минимальный PDF с /Info и XMP-потоком.
fn pdf_with_info(dir: &Path) -> PathBuf {
    let path = dir.join("doc.pdf");
    let mut body = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let objects = [
        (1_u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        (
            3,
            "<< /Author (Ivan Petrov) /Creator (Word) /Title (Report) >>",
        ),
        (
            4,
            "<< /Type /Metadata /Subtype /XML /Length 20 >>\nstream\n<x:xmpmeta>secret</x:xmpmeta>\nendstream",
        ),
    ];
    for (number, content) in objects {
        offsets.push(body.len());
        body.extend_from_slice(format!("{number} 0 obj\n{content}\nendobj\n").as_bytes());
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
    fs::write(&path, body).unwrap();
    path
}

fn part(path: &Path, name: &str) -> String {
    let file = fs::File::open(path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut archive.by_name(name).unwrap(), &mut text).unwrap();
    text
}

#[test]
fn clean_removes_png_text_and_keeps_the_picture_working() {
    let dir = tempfile::tempdir().unwrap();
    let input = png_with_text(dir.path());
    let original = fs::read(&input).unwrap();
    assert!(original.windows(4).any(|window| window == b"tEXt"));

    let code = cli(&["clean", path_str(&input), "--no-progress"]);
    assert_eq!(code, 0);

    let cleaned = dir.path().join("shot_clean.png");
    let bytes = fs::read(&cleaned).unwrap();
    assert!(!bytes.windows(4).any(|window| window == b"tEXt"));
    assert!(image::load_from_memory(&bytes).is_ok());
    // Исходник не перезаписан.
    assert_eq!(fs::read(&input).unwrap(), original);
    // Файл действительно уменьшился на размер чанка.
    assert!(bytes.len() < original.len());
}

#[test]
fn dry_run_lists_metadata_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = png_with_text(dir.path());
    let code = cli(&["clean", path_str(&input), "--dry-run"]);
    assert_eq!(code, 0);
    assert!(!dir.path().join("shot_clean.png").exists());
}

#[test]
fn clean_clears_office_properties_and_package_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let input = docx_with_properties(dir.path());
    let out_dir = dir.path().join("out");
    let code = cli(&[
        "clean",
        path_str(&input),
        "--out-dir",
        path_str(&out_dir),
        "--no-progress",
    ]);
    assert_eq!(code, 0);

    let output = out_dir.join("report_clean.docx");
    let core = part(&output, "docProps/core.xml");
    assert!(!core.contains("Иван"));
    assert!(!core.contains("Отчёт"));
    assert!(core.contains("coreProperties"));
    // Основная часть на месте, пакет читается.
    assert!(part(&output, "word/document.xml").contains("document"));
}

#[test]
fn clean_strips_pdf_info_and_metadata_keys() {
    let dir = tempfile::tempdir().unwrap();
    let input = pdf_with_info(dir.path());
    let code = cli(&["clean", path_str(&input), "--no-progress"]);
    assert_eq!(code, 0);

    let output = dir.path().join("doc_clean.pdf");
    let text = String::from_utf8_lossy(&fs::read(&output).unwrap()).into_owned();
    assert!(!text.contains("/Info 3 0 R"));
    assert!(!text.contains("/Metadata 4 0 R"));
    assert!(!text.contains("Ivan Petrov"));
    assert!(text.starts_with("%PDF-1.4"));
    assert!(text.trim_end().ends_with("%%EOF"));
}

#[test]
fn unknown_format_is_reported_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("archive.zip");
    let mut writer = ZipWriter::new(fs::File::create(&input).unwrap());
    writer
        .start_file("note.txt", FileOptions::default())
        .unwrap();
    writer.write_all(b"hello").unwrap();
    writer.finish().unwrap();

    let code = cli(&["clean", path_str(&input), "--no-progress"]);
    assert_eq!(code, 1);
    assert!(!dir.path().join("archive_clean.zip").exists());
}

#[test]
fn suffix_and_profiles_options_are_respected() {
    let dir = tempfile::tempdir().unwrap();
    let input = png_with_text(dir.path());
    let code = cli(&[
        "clean",
        path_str(&input),
        "--suffix",
        "-no-meta",
        "--profiles",
        "--no-progress",
    ]);
    assert_eq!(code, 0);
    assert!(dir.path().join("shot-no-meta.png").is_file());
}
