//! Удаление метаданных из готовых файлов.
//!
//! Поддержаны форматы, которые программа и так разбирает: PNG, JPEG, GIF,
//! WebP, SVG, пакеты Office (`docx`, `xlsx`, `pptx`) и PDF. Для картинок и
//! документов удаляются только «внешние» блоки — текст, EXIF/XMP/IPTC,
//! комментарии, свойства документа, — поэтому файл продолжает открываться.
//! Содержимое (пиксели, текст документа) не меняется.
//!
//! Одна и та же обходная логика используется и для очистки, и для обзора
//! «что найдено»: обзор просто пишет результат в счётчик вместо файла.

mod flac;
mod gif;
mod jpeg;
mod mp3;
mod opc;
mod pdf;
mod png;
mod svg;
mod webp;

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::util::Job;

pub use opc::OPC_CONTENT_TYPES;

/// Что за файл перед нами.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
    /// Пакет OPC: `docx`, `xlsx`, `pptx` и родственные.
    Opc,
    Pdf,
    /// MP3-аудио: ID3v2 в начале, ID3v1 и APE в конце.
    Mp3,
    /// FLAC-аудио: комментарии Vorbis и картинки в блоках метаданных.
    Flac,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Png,
        Kind::Jpeg,
        Kind::Gif,
        Kind::Webp,
        Kind::Svg,
        Kind::Opc,
        Kind::Pdf,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            Kind::Jpeg => "JPEG",
            Kind::Gif => "GIF",
            Kind::Webp => "WebP",
            Kind::Svg => "SVG",
            Kind::Opc => "пакет Office",
            Kind::Pdf => "PDF",
            Kind::Mp3 => "MP3",
            Kind::Flac => "FLAC",
        }
    }
}

/// Что удалять. Пиксели, текст документа и полезную нагрузку FilePumper
/// эти флаги не затрагивают.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanOptions {
    /// Текст, EXIF/XMP/IPTC, комментарии, время изменения, свойства документа.
    pub text: bool,
    /// Цветовые профили (ICC) и встроенные превью документов.
    pub profiles: bool,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            text: true,
            profiles: false,
        }
    }
}

impl CleanOptions {
    /// Всё, что программа умеет удалять — для обзора «что найдено».
    pub fn everything() -> Self {
        Self {
            text: true,
            profiles: true,
        }
    }

    /// Ничего не удалять: нужен, чтобы просто проверить файл.
    pub fn nothing(self) -> bool {
        !self.text && !self.profiles
    }
}

/// Один удалённый блок метаданных.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// Что именно убрали: `PNG tEXt «Comment»`, `PDF /Info`, …
    pub label: String,
    /// Сколько байт занимал блок вместе со служебными полями.
    pub bytes: u64,
}

impl Removal {
    pub fn new(label: impl Into<String>, bytes: u64) -> Self {
        Self {
            label: label.into(),
            bytes,
        }
    }
}

/// Итог обзора или очистки одного файла.
#[derive(Clone, Debug)]
pub struct Report {
    pub kind: Kind,
    pub removed: Vec<Removal>,
    pub size_before: u64,
    pub size_after: u64,
}

impl Report {
    pub fn empty(kind: Kind, size: u64) -> Self {
        Self {
            kind,
            removed: Vec::new(),
            size_before: size,
            size_after: size,
        }
    }

    pub fn removed_bytes(&self) -> u64 {
        self.removed.iter().map(|removal| removal.bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }

    /// Короткая строка для списка файлов: `PNG: 3 блока, 1.2 КБ`.
    pub fn blocks_label(&self) -> String {
        match self.removed.len() {
            0 => format!("{}: метаданных нет", self.kind.label()),
            count => format!(
                "{}: {} {}, {}",
                self.kind.label(),
                count,
                plural_blocks(count),
                crate::util::format_bytes(self.removed_bytes() as f64)
            ),
        }
    }

    /// Подробный отчёт для окна «Готово».
    pub fn summary(&self) -> String {
        let mut text = if self.removed.is_empty() {
            format!(
                "{}: метаданных не найдено, файл скопирован без изменений.",
                self.kind.label()
            )
        } else {
            format!(
                "{}: удалено {} {}, {}.",
                self.kind.label(),
                self.removed.len(),
                plural_blocks(self.removed.len()),
                crate::util::format_bytes(self.removed_bytes() as f64)
            )
        };
        for removal in &self.removed {
            text.push_str(&format!(
                "\n  {} — {}",
                removal.label,
                crate::util::format_bytes(removal.bytes as f64)
            ));
        }
        if self.size_before != self.size_after && self.size_after != 0 {
            text.push_str(&format!(
                "\nРазмер: {} → {}",
                crate::util::format_bytes(self.size_before as f64),
                crate::util::format_bytes(self.size_after as f64)
            ));
        }
        text
    }
}

fn plural_blocks(count: usize) -> &'static str {
    match count % 10 {
        1 if count % 100 != 11 => "блок",
        2..=4 if !(12..=14).contains(&(count % 100)) => "блока",
        _ => "блоков",
    }
}

/// Определяет формат файла по содержимому, расширение — только для SVG.
pub fn detect(path: &Path) -> Result<Kind, String> {
    let mut head = vec![0_u8; 4096];
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) => return Err(err.to_string()),
        }
    }
    head.truncate(filled);
    detect_head(path, &head)
}

fn detect_head(path: &Path, head: &[u8]) -> Result<Kind, String> {
    if head.starts_with(&crate::payload::carrier::PNG_SIGNATURE) {
        return Ok(Kind::Png);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(Kind::Jpeg);
    }
    if head.starts_with(b"GIF8") {
        return Ok(Kind::Gif);
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Ok(Kind::Webp);
    }
    if head.starts_with(b"%PDF") {
        return Ok(Kind::Pdf);
    }
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        return Ok(Kind::Mp3);
    }
    if head.starts_with(b"fLaC") {
        return Ok(Kind::Flac);
    }
    if head.starts_with(b"PK\x03\x04") {
        return if opc::is_package(path) {
            Ok(Kind::Opc)
        } else {
            Err("Обычный ZIP-архив метаданных не содержит: поддержаны PNG, JPEG, GIF, WebP, SVG, docx/xlsx/pptx, PDF, MP3 и FLAC.".to_owned())
        };
    }
    if svg::looks_like_svg(path, head) {
        return Ok(Kind::Svg);
    }
    Err(format!(
        "Формат не распознан: поддержаны PNG, JPEG, GIF, WebP, SVG, docx/xlsx/pptx и PDF ({}).",
        path.display()
    ))
}

/// Обзор метаданных: что было бы удалено с выбранными опциями.
pub fn scan(path: &Path, options: CleanOptions) -> Result<Report, String> {
    let kind = detect(path)?;
    let size = fs::metadata(path).map_err(|err| err.to_string())?.len();
    let mut report = Report::empty(kind, size);
    let mut counter = Counter::default();
    walk(
        kind,
        path,
        &mut counter,
        options,
        &mut report,
        &Job::quiet(),
    )?;
    Ok(report)
}

/// Записывает в `output` копию файла без метаданных.
pub fn clean(
    input: &Path,
    output: &Path,
    options: CleanOptions,
    job: &Job<'_>,
) -> Result<Report, String> {
    job.check()?;
    let kind = detect(input)?;
    let size = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let mut report = Report::empty(kind, size);
    let file = File::create(output).map_err(|err| err.to_string())?;
    let mut writer = BufWriter::new(file);
    walk(kind, input, &mut writer, options, &mut report, job)?;
    writer.flush().map_err(|err| err.to_string())?;
    drop(writer);
    report.size_after = fs::metadata(output).map_err(|err| err.to_string())?.len();
    job.report(100.0, "Метаданные удалены.");
    Ok(report)
}

fn walk(
    kind: Kind,
    input: &Path,
    output: &mut dyn SeekWrite,
    options: CleanOptions,
    report: &mut Report,
    job: &Job<'_>,
) -> Result<(), String> {
    match kind {
        Kind::Png => png::clean(input, output, options, report, job),
        Kind::Jpeg => jpeg::clean(input, output, options, report, job),
        Kind::Gif => gif::clean(input, output, options, report, job),
        Kind::Webp => webp::clean(input, output, options, report, job),
        Kind::Svg => svg::clean(input, output, options, report, job),
        Kind::Opc => opc::clean(input, output, options, report, job),
        Kind::Pdf => pdf::clean(input, output, options, report, job),
        Kind::Mp3 => mp3::clean(input, output, options, report, job),
        Kind::Flac => flac::clean(input, output, options, report, job),
    }
}

/// Приёмник результата: нужны и запись, и переход по уже записанным байтам
/// (WebP и Office правят размеры в заголовке после записи).
pub trait SeekWrite: Write + Seek {}
impl<T: Write + Seek> SeekWrite for T {}

/// Счётчик вместо файла: обзор не пишет копию, но идёт тем же путём.
#[derive(Default)]
struct Counter {
    bytes: u64,
}

impl Write for Counter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes += buffer.len() as u64;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Counter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match position {
            SeekFrom::Start(value) => self.bytes = value,
            SeekFrom::Current(delta) => {
                self.bytes = self.bytes.saturating_add_signed(delta);
            }
            SeekFrom::End(_) => {}
        }
        Ok(self.bytes)
    }
}

/// Прогресс по байтам файла: считает прочитанное и проверяет отмену.
pub(super) struct Progress<'a, 'b> {
    total: u64,
    done: u64,
    job: &'a Job<'b>,
}

impl<'a, 'b> Progress<'a, 'b> {
    pub(super) fn new(total: u64, job: &'a Job<'b>) -> Self {
        Self {
            total,
            done: 0,
            job,
        }
    }

    pub(super) fn advance(&mut self, bytes: u64) -> Result<(), String> {
        self.done = self.done.saturating_add(bytes);
        self.job.check()?;
        if self.total > 0 {
            self.job.report(
                (self.done as f32 / self.total as f32 * 100.0).min(99.0),
                "Очистка метаданных...",
            );
        }
        Ok(())
    }
}

/// Читает ровно `bytes` байт во временный буфер и отдаёт их.
pub(super) fn read_block<R: Read>(input: &mut R, bytes: u64) -> Result<Vec<u8>, String> {
    let size = usize::try_from(bytes).map_err(|_| "Слишком большой блок файла.".to_owned())?;
    let mut buffer = vec![0_u8; size];
    input
        .read_exact(&mut buffer)
        .map_err(|_| "Файл обрезан: неожиданный конец данных.".to_owned())?;
    Ok(buffer)
}

/// Копирует `bytes` байт в выход, не держа их целиком в памяти.
pub(super) fn copy_exact<R: Read, W: Write + ?Sized>(
    input: &mut R,
    output: &mut W,
    bytes: u64,
    progress: &mut Progress<'_, '_>,
) -> Result<(), String> {
    let mut left = bytes;
    let mut buffer = vec![0_u8; 256 * 1024];
    while left > 0 {
        let want = left.min(buffer.len() as u64) as usize;
        input
            .read_exact(&mut buffer[..want])
            .map_err(|_| "Файл обрезан: неожиданный конец данных.".to_owned())?;
        output
            .write_all(&buffer[..want])
            .map_err(|err| err.to_string())?;
        left -= want as u64;
        progress.advance(want as u64)?;
    }
    Ok(())
}

/// Пропускает `bytes` байт, только читая их (для удаляемых блоков).
pub(super) fn skip_exact<R: Read>(
    input: &mut R,
    bytes: u64,
    progress: &mut Progress<'_, '_>,
) -> Result<(), String> {
    copy_exact(input, &mut io::sink(), bytes, progress)
}

pub(super) fn open_input(path: &Path) -> Result<BufReader<File>, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    Ok(BufReader::with_capacity(256 * 1024, file))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pumper::{generate, GeneratedKind};

    /// Сгенерированные картинки не несут своих метаданных. Исключение — GIF:
    /// размер в нём добит Comment Extension, а это честные метаданные, поэтому
    /// очистка их видит и убирает.
    #[test]
    fn generated_images_have_no_metadata_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        for kind in [
            GeneratedKind::Png,
            GeneratedKind::Jpeg,
            GeneratedKind::Gif,
            GeneratedKind::Webp,
        ] {
            let path = dir.path().join(format!("made.{}", kind.extension()));
            generate::run(
                kind,
                &path,
                64 * 1024,
                generate::ImageSize::default(),
                crate::pumper::generate::NoiseKind::default(),
                &Job::quiet(),
            )
            .unwrap();
            let report = scan(&path, CleanOptions::everything()).unwrap();
            if kind == GeneratedKind::Gif {
                assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
                assert!(report.removed[0].label.contains("комментарий"));
            } else {
                assert!(report.is_empty(), "{}: {:?}", kind.title(), report.removed);
            }
        }
    }
}
