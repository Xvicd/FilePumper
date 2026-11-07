//! Полезная нагрузка внутри файлов-носителей:
//! упаковка, встраивание, проверка результата и извлечение.

pub mod carrier;
pub mod codes;
pub mod format;
pub mod gif;
pub mod jpeg;
pub mod png;
pub mod raw;
pub mod svg;
pub mod webp;

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub use carrier::{detect_carrier, Carrier};
pub use format::{ContainerOptions, HeaderInfo, ParsedContainer, PayloadInfo};

use crate::util::{same_file, sanitize_file_name, Job};

#[derive(Clone, Debug)]
pub struct EmbedOutcome {
    pub output: PathBuf,
    pub carrier: Carrier,
    pub copies: usize,
    pub output_len: u64,
    pub options: String,
    pub verified_with_decoder: bool,
}

#[derive(Clone, Debug)]
pub struct ExtractOutcome {
    pub output: PathBuf,
    pub name: String,
    pub size: u64,
    pub copies: usize,
}

#[derive(Clone, Debug)]
pub struct PayloadSummary {
    pub info: PayloadInfo,
    pub carrier: Carrier,
}

fn read_stack(carrier: &Path, kind: Carrier) -> Result<Vec<u8>, String> {
    match kind {
        Carrier::Png => png::extract(carrier),
        Carrier::Jpeg => jpeg::extract(carrier),
        Carrier::Gif => gif::extract(carrier),
        Carrier::Webp => webp::extract(carrier),
        Carrier::Svg => svg::extract(carrier),
        Carrier::Raw => raw::extract(carrier),
    }
}

/// Режет стек копий на куски, не выходящие за лимит формата и не склеивая
/// две копии в один кусок: тогда обрезка файла убивает только последнюю копию.
pub fn segments(data: &[u8], copy_len: usize, limit: usize) -> Vec<&[u8]> {
    let copy_len = copy_len.max(1);
    let limit = limit.max(1);
    let mut result = Vec::new();
    for copy in data.chunks(copy_len) {
        for piece in copy.chunks(limit) {
            result.push(piece);
        }
    }
    result
}

fn write_stack(
    kind: Carrier,
    carrier: &Path,
    data: &[u8],
    copy_len: usize,
    output: &Path,
    job: &Job<'_>,
) -> Result<(), String> {
    match kind {
        Carrier::Png => png::embed(carrier, data, copy_len, output, job),
        Carrier::Jpeg => jpeg::embed(carrier, data, copy_len, output, job),
        Carrier::Gif => gif::embed(carrier, data, copy_len, output),
        Carrier::Webp => webp::embed(carrier, data, copy_len, output),
        Carrier::Svg => svg::embed(carrier, data, copy_len, output),
        Carrier::Raw => raw::embed(carrier, data, output),
    }
}

/// Проверяет, что записанный файл по-прежнему полностью декодируется.
/// Именно это проверяют серверы и мессенджеры, отбрасывая «битые» картинки.
fn decodes_cleanly(path: &Path, kind: Carrier) -> bool {
    match kind {
        Carrier::Png | Carrier::Jpeg | Carrier::Gif | Carrier::Webp => image::open(path).is_ok(),
        Carrier::Svg => fs::read_to_string(path)
            .map(|text| text.trim_end().ends_with("</svg>"))
            .unwrap_or(false),
        Carrier::Raw => true,
    }
}

/// Имя файла результата рядом с носителем: `photo.png` → `photo_with_payload.png`.
pub fn default_output_path(carrier: &Path) -> PathBuf {
    let stem = carrier
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("carrier");
    let name = match carrier.extension().and_then(|value| value.to_str()) {
        Some(extension) => format!("{stem}_with_payload.{extension}"),
        None => format!("{stem}_with_payload"),
    };
    carrier.with_file_name(name)
}

/// Путь для извлечения: рядом с носителем, но никогда не поверх него.
pub fn default_extract_path(carrier: &Path, name: &str) -> PathBuf {
    let safe = sanitize_file_name(name);
    let candidate = carrier.with_file_name(&safe);
    if same_file(carrier, &candidate) {
        return carrier.with_file_name(format!("extracted_{safe}"));
    }
    candidate
}

/// Встраивает файл в носитель, повторяя контейнер `copies` раз и проверяя результат.
pub fn embed_file(
    carrier: &Path,
    payload: &Path,
    output: &Path,
    options: &ContainerOptions,
    copies: usize,
    job: &Job<'_>,
) -> Result<EmbedOutcome, String> {
    let copies = copies.clamp(format::MIN_COPIES, format::MAX_COPIES);
    if same_file(carrier, output) {
        return Err("Итоговый файл совпадает с носителем. Выберите другое имя.".into());
    }
    if same_file(payload, output) {
        return Err("Итоговый файл совпадает с файлом нагрузки. Выберите другое имя.".into());
    }
    let kind = detect_carrier(carrier)?;
    let display_name = payload
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("payload.bin");
    let payload_size = fs::metadata(payload).map_err(|err| err.to_string())?.len();
    if payload_size > format::MAX_CONTAINER_BYTES as u64 {
        return Err(format!(
            "Файл нагрузки больше {} МБ — уменьшите его.",
            format::MAX_CONTAINER_BYTES / (1024 * 1024)
        ));
    }
    job.check()?;
    job.report(2.0, &format!("Чтение {display_name}..."));
    let mut bytes = Vec::with_capacity(payload_size as usize);
    BufReader::new(File::open(payload).map_err(|err| err.to_string())?)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;

    let container = format::build_container(&bytes, display_name, options, job)?;
    let mut stack = Vec::with_capacity(container.len() * copies);
    for _ in 0..copies {
        stack.extend_from_slice(&container);
    }
    job.check()?;
    job.report(
        35.0,
        &format!("Встраивание в {} ({} копия(и))...", kind.label(), copies),
    );
    write_stack(kind, carrier, &stack, container.len(), output, job)?;

    job.report(92.0, "Проверка записанного файла...");
    let written_len = fs::metadata(output).map_err(|err| err.to_string())?.len();
    let stack_back = read_stack(output, kind)?;
    let (_parsed, found) = format::pick_container(&stack_back, options.password())?;
    // У обычных файлов читается окно хвоста (64 МБ), поэтому гигантские копии
    // может увидеть только путь по концевому маркеру — для них достаточно одной.
    let required = if kind == Carrier::Raw && stack_back.len() as u64 == written_len {
        1
    } else {
        copies
    };
    if found < required {
        return Err(format!(
            "После записи найдено только {found} из {copies} копий — файл-носитель слишком сильно изменился."
        ));
    }
    let verified_with_decoder = decodes_cleanly(output, kind);
    job.report(100.0, "Готово.");
    Ok(EmbedOutcome {
        output: output.to_path_buf(),
        carrier: kind,
        copies,
        output_len: written_len,
        options: options.describe(),
        verified_with_decoder,
    })
}

/// Читает контейнер и проверяет контрольные суммы (без записи на диск).
pub fn extract_payload(
    carrier: &Path,
    password: Option<&str>,
) -> Result<(ParsedContainer, usize, Carrier), String> {
    let kind = detect_carrier(carrier)?;
    let stack = read_stack(carrier, kind)?;
    let (parsed, copies) = format::pick_container(&stack, password)?;
    Ok((parsed, copies, kind))
}

/// Сводка для окна предварительного просмотра — пароль не требуется.
pub fn inspect_file(carrier: &Path, password: Option<&str>) -> Result<PayloadSummary, String> {
    let kind = detect_carrier(carrier)?;
    let stack = read_stack(carrier, kind)?;
    let headers = format::peek_headers(&stack);
    let Some(primary) = headers.first() else {
        return Err(format::PAYLOAD_NOT_FOUND.to_owned());
    };
    let info = PayloadInfo {
        name: sanitize_file_name(&primary.name),
        size: primary.plain_len,
        copies: headers.len(),
        compressed: primary.compressed,
        encrypted: primary.encrypted,
        legacy: primary.legacy,
    };
    if info.encrypted {
        if let Some(password) = password.map(str::trim).filter(|value| !value.is_empty()) {
            // Немедленно сообщаем о неверном пароле.
            format::pick_container(&stack, Some(password))?;
        }
    }
    Ok(PayloadSummary {
        info,
        carrier: kind,
    })
}

/// Извлекает нагрузку в указанный файл.
pub fn extract_to(
    carrier: &Path,
    destination: &Path,
    password: Option<&str>,
    job: &Job<'_>,
) -> Result<ExtractOutcome, String> {
    if same_file(carrier, destination) {
        return Err("Нельзя перезаписать файл-носитель результатом извлечения.".into());
    }
    job.check()?;
    job.report(5.0, "Поиск полезной нагрузки...");
    let (parsed, copies, _kind) = extract_payload(carrier, password)?;
    job.check()?;
    job.report(60.0, "Запись извлечённого файла...");
    let mut writer = BufWriter::new(File::create(destination).map_err(|err| err.to_string())?);
    writer
        .write_all(&parsed.payload)
        .map_err(|err| err.to_string())?;
    writer.flush().map_err(|err| err.to_string())?;
    job.report(100.0, "Готово.");
    Ok(ExtractOutcome {
        output: destination.to_path_buf(),
        name: sanitize_file_name(&parsed.name),
        size: parsed.payload.len() as u64,
        copies,
    })
}
