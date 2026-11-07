//! Формат контейнера полезной нагрузки.
//!
//! Версия 2: `UFPL` | версия | флаги | длина имени | имя | длина блоба | исходная
//! длина | SHA-256 блоба | [параметры шифрования] | CRC32 заголовка | блоб | `LFPU`
//! | полная длина. Версия 1 (без SHA-256 и без флагов) читается для совместимости.

use crate::payload::codes::{self, CipherParams, NONCE_LEN, SALT_LEN};
use crate::util::{crc32_bytes, read_u32_be, read_u64_be, Job};

pub const CONTAINER_MAGIC: &[u8; 4] = b"UFPL";
pub const CONTAINER_FOOTER: &[u8; 4] = b"LFPU";
pub const CONTAINER_VERSION: u8 = 2;
pub const LEGACY_VERSION: u8 = 1;
pub const FLAG_COMPRESSED: u8 = 0b0000_0001;
pub const FLAG_ENCRYPTED: u8 = 0b0000_0010;
pub const KNOWN_FLAGS: u8 = FLAG_COMPRESSED | FLAG_ENCRYPTED;
pub const MAX_CONTAINER_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_COPIES: usize = 3;
pub const MIN_COPIES: usize = 1;
/// Верхняя граница суммарного объёма копий, которые читаются из носителя.
pub const MAX_STACK_BYTES: usize = MAX_CONTAINER_BYTES * MAX_COPIES;
pub const PAYLOAD_NOT_FOUND: &str = "Полезная нагрузка в этом файле не найдена.";
const FOOTER_LEN: usize = 12;

#[derive(Clone, Debug, Default)]
pub struct ContainerOptions {
    pub compress: bool,
    pub password: Option<String>,
}

impl ContainerOptions {
    pub fn password(&self) -> Option<&str> {
        self.password
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.compress {
            parts.push("сжатие deflate".to_owned());
        }
        if self.password().is_some() {
            parts.push("шифрование паролем".to_owned());
        }
        if parts.is_empty() {
            "без сжатия и шифрования".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParsedContainer {
    pub name: String,
    pub payload: Vec<u8>,
    pub compressed: bool,
    pub encrypted: bool,
    pub legacy: bool,
}

#[derive(Clone, Debug)]
pub struct PayloadInfo {
    pub name: String,
    pub size: u64,
    pub copies: usize,
    pub compressed: bool,
    pub encrypted: bool,
    pub legacy: bool,
}

fn truncated_header() -> String {
    "Полезная нагрузка повреждена: обрезанный заголовок.".to_owned()
}

#[derive(Clone, Debug)]
pub struct HeaderInfo {
    pub name: String,
    pub plain_len: u64,
    pub compressed: bool,
    pub encrypted: bool,
    pub legacy: bool,
}

struct HeaderView {
    info: HeaderInfo,
    payload_start: usize,
    stored_len: usize,
    sha: Option<[u8; 32]>,
    legacy_crc: Option<u32>,
    params: Option<CipherParams>,
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    let slice = bytes.get(offset..offset + N)?;
    let mut value = [0_u8; N];
    value.copy_from_slice(slice);
    Some(value)
}

/// Разбирает только заголовок: позволяет показать имя и размер до ввода пароля.
fn view_header(bytes: &[u8]) -> Result<HeaderView, String> {
    if bytes.len() < 8 {
        return Err(truncated_header());
    }
    if &bytes[0..4] != CONTAINER_MAGIC {
        return Err(PAYLOAD_NOT_FOUND.to_owned());
    }
    let version = bytes[4];
    let flags = bytes[5];
    if flags & !KNOWN_FLAGS != 0 {
        return Err("Полезная нагрузка использует неподдерживаемые флаги.".into());
    }
    let name_len = read_u16_be(bytes, 6).ok_or_else(truncated_header)? as usize;
    let name_end = 8_usize.checked_add(name_len).ok_or_else(truncated_header)?;
    let raw_name = bytes.get(8..name_end).ok_or_else(truncated_header)?;
    let name = String::from_utf8_lossy(raw_name).into_owned();

    match version {
        CONTAINER_VERSION => {
            let stored_len = read_u64_be(bytes, name_end).ok_or_else(truncated_header)?;
            let plain_len = read_u64_be(bytes, name_end + 8).ok_or_else(truncated_header)?;
            let sha = read_array::<32>(bytes, name_end + 16).ok_or_else(truncated_header)?;
            let mut cursor = name_end + 48;
            let mut params = None;
            if flags & FLAG_ENCRYPTED != 0 {
                let salt = read_array::<SALT_LEN>(bytes, cursor).ok_or_else(truncated_header)?;
                cursor += SALT_LEN;
                let nonce = read_array::<NONCE_LEN>(bytes, cursor).ok_or_else(truncated_header)?;
                cursor += NONCE_LEN;
                let m_cost = read_u32_be(bytes, cursor).ok_or_else(truncated_header)?;
                let t_cost = read_u32_be(bytes, cursor + 4).ok_or_else(truncated_header)?;
                let p_cost = read_u32_be(bytes, cursor + 8).ok_or_else(truncated_header)?;
                cursor += 12;
                let parsed = CipherParams {
                    salt,
                    nonce,
                    m_cost,
                    t_cost,
                    p_cost,
                };
                parsed.validate()?;
                params = Some(parsed);
            }
            let header_crc = read_u32_be(bytes, cursor).ok_or_else(truncated_header)?;
            if crc32_bytes(&bytes[..cursor]) != header_crc {
                return Err("Полезная нагрузка повреждена: CRC заголовка не совпадает.".into());
            }
            Ok(HeaderView {
                info: HeaderInfo {
                    name,
                    plain_len,
                    compressed: flags & FLAG_COMPRESSED != 0,
                    encrypted: flags & FLAG_ENCRYPTED != 0,
                    legacy: false,
                },
                payload_start: cursor + 4,
                stored_len: usize::try_from(stored_len).map_err(|_| {
                    "Полезная нагрузка слишком велика для этой платформы.".to_owned()
                })?,
                sha: Some(sha),
                legacy_crc: None,
                params,
            })
        }
        LEGACY_VERSION => {
            let stored_len = read_u64_be(bytes, name_end).ok_or_else(truncated_header)?;
            let payload_crc = read_u32_be(bytes, name_end + 8).ok_or_else(truncated_header)?;
            let header_crc = read_u32_be(bytes, name_end + 12).ok_or_else(truncated_header)?;
            if crc32_bytes(&bytes[..name_end + 12]) != header_crc {
                return Err("Полезная нагрузка повреждена: CRC заголовка не совпадает.".into());
            }
            let stored_len = usize::try_from(stored_len)
                .map_err(|_| "Полезная нагрузка слишком велика для этой платформы.".to_owned())?;
            Ok(HeaderView {
                info: HeaderInfo {
                    name,
                    plain_len: stored_len as u64,
                    compressed: false,
                    encrypted: false,
                    legacy: true,
                },
                payload_start: name_end + 16,
                stored_len,
                sha: None,
                legacy_crc: Some(payload_crc),
                params: None,
            })
        }
        other => Err(format!(
            "Неподдерживаемая версия полезной нагрузки: {other}."
        )),
    }
}

/// Имя, размер и флаги без расшифровки — для окна предварительного просмотра.
pub fn peek_header(bytes: &[u8]) -> Result<HeaderInfo, String> {
    Ok(view_header(bytes)?.info)
}

/// Полный разбор контейнера: проверка длины, контрольной суммы и расшифровка.
pub fn parse_container(bytes: &[u8], password: Option<&str>) -> Result<ParsedContainer, String> {
    let view = view_header(bytes)?;
    let payload_end = view
        .payload_start
        .checked_add(view.stored_len)
        .ok_or_else(|| "Полезная нагрузка повреждена: неверная длина.".to_owned())?;
    let total_len = payload_end
        .checked_add(FOOTER_LEN)
        .ok_or_else(|| "Полезная нагрузка повреждена: неверная длина.".to_owned())?;
    if bytes.len() < total_len {
        return Err(format!(
            "Полезная нагрузка обрезана: ожидалось {total_len} байт, получено {}.",
            bytes.len()
        ));
    }
    if &bytes[payload_end..payload_end + 4] != CONTAINER_FOOTER {
        return Err("Полезная нагрузка повреждена: не найден концевой маркер.".into());
    }
    if read_u64_be(bytes, payload_end + 4) != Some(total_len as u64) {
        return Err("Полезная нагрузка повреждена: неверная длина контейнера.".into());
    }
    let stored = &bytes[view.payload_start..payload_end];
    if let Some(expected) = view.legacy_crc {
        if crc32_bytes(stored) != expected {
            return Err("Полезная нагрузка повреждена: CRC данных не совпадает.".into());
        }
    }
    if let Some(expected) = view.sha {
        if codes::sha256(stored) != expected {
            return Err("Полезная нагрузка повреждена: SHA-256 данных не совпадает.".into());
        }
    }

    let mut blob = stored.to_vec();
    if view.info.encrypted {
        let params = view
            .params
            .as_ref()
            .ok_or_else(|| "Полезная нагрузка повреждена: нет параметров шифрования.".to_owned())?;
        let password = password
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Полезная нагрузка зашифрована: требуется пароль.".to_owned())?;
        blob = codes::decrypt(&blob, password, params)?;
    }
    let payload = if view.info.compressed {
        let expected = usize::try_from(view.info.plain_len)
            .map_err(|_| "Полезная нагрузка слишком велика для этой платформы.".to_owned())?;
        codes::decompress(&blob, expected)?
    } else {
        if blob.len() as u64 != view.info.plain_len {
            return Err("Полезная нагрузка повреждена: размер не совпадает с заголовком.".into());
        }
        blob
    };
    if payload.len() > MAX_CONTAINER_BYTES {
        return Err("Полезная нагрузка превышает допустимый размер.".into());
    }
    Ok(ParsedContainer {
        name: view.info.name,
        payload,
        compressed: view.info.compressed,
        encrypted: view.info.encrypted,
        legacy: view.info.legacy,
    })
}

fn find_magic(haystack: &[u8]) -> Option<usize> {
    haystack
        .windows(CONTAINER_MAGIC.len())
        .position(|window| window == CONTAINER_MAGIC)
}

/// Ищет все копии контейнера в собранных байтах. Возвращает первую целую копию
/// и общее количество целых копий — так нагрузка выживает, если сервер обрезал хвост.
pub fn pick_container(
    bytes: &[u8],
    password: Option<&str>,
) -> Result<(ParsedContainer, usize), String> {
    let mut first: Option<ParsedContainer> = None;
    let mut copies = 0_usize;
    let mut first_error: Option<String> = None;
    let mut offset = 0_usize;
    while let Some(relative) = find_magic(&bytes[offset..]) {
        let start = offset + relative;
        match parse_container(&bytes[start..], password) {
            Ok(parsed) => {
                copies += 1;
                if first.is_none() {
                    first = Some(parsed);
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        offset = start + CONTAINER_MAGIC.len();
    }
    match first {
        Some(parsed) => Ok((parsed, copies)),
        None => Err(first_error.unwrap_or_else(|| PAYLOAD_NOT_FOUND.to_owned())),
    }
}

/// Заголовки всех найденных копий (без пароля и без расшифровки).
pub fn peek_headers(bytes: &[u8]) -> Vec<HeaderInfo> {
    let mut headers = Vec::new();
    let mut offset = 0_usize;
    while let Some(relative) = find_magic(&bytes[offset..]) {
        let start = offset + relative;
        if let Ok(info) = peek_header(&bytes[start..]) {
            headers.push(info);
        }
        offset = start + CONTAINER_MAGIC.len();
    }
    headers
}

/// Сводка для интерфейса: имя, размер, число целых копий.
pub fn summarize(bytes: &[u8], password: Option<&str>) -> Result<PayloadInfo, String> {
    let (parsed, copies) = pick_container(bytes, password)?;
    Ok(PayloadInfo {
        name: parsed.name,
        size: parsed.payload.len() as u64,
        copies,
        compressed: parsed.compressed,
        encrypted: parsed.encrypted,
        legacy: parsed.legacy,
    })
}

/// Упаковывает файл в контейнер: при необходимости сжимает и шифрует.
pub fn build_container(
    payload: &[u8],
    display_name: &str,
    options: &ContainerOptions,
    job: &Job<'_>,
) -> Result<Vec<u8>, String> {
    job.check()?;
    if payload.len() > MAX_CONTAINER_BYTES {
        return Err(format!(
            "Полезная нагрузка больше {} МБ и не помещается в контейнер.",
            MAX_CONTAINER_BYTES / (1024 * 1024)
        ));
    }
    let mut flags = 0_u8;
    let mut blob = payload.to_vec();
    if options.compress {
        job.check()?;
        job.report(8.0, "Сжатие полезной нагрузки...");
        let packed = codes::compress(payload)?;
        // Сжатие применяем только если оно реально что-то даёт.
        if packed.len() + 16 < payload.len() {
            blob = packed;
            flags |= FLAG_COMPRESSED;
        }
    }
    let mut cipher_params = None;
    if let Some(password) = options.password() {
        job.check()?;
        job.report(16.0, "Шифрование: Argon2id + ChaCha20-Poly1305...");
        let (encrypted, params) = codes::encrypt(&blob, password)?;
        blob = encrypted;
        cipher_params = Some(params);
        flags |= FLAG_ENCRYPTED;
    }

    job.report(22.0, "Расчёт контрольных сумм...");
    let name = display_name.as_bytes();
    let name_len =
        u16::try_from(name.len()).map_err(|_| "Слишком длинное имя файла нагрузки.".to_owned())?;
    let mut header = Vec::with_capacity(64 + name.len());
    header.extend_from_slice(CONTAINER_MAGIC);
    header.push(CONTAINER_VERSION);
    header.push(flags);
    header.extend_from_slice(&name_len.to_be_bytes());
    header.extend_from_slice(name);
    header.extend_from_slice(&(blob.len() as u64).to_be_bytes());
    header.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    header.extend_from_slice(&codes::sha256(&blob));
    if let Some(params) = cipher_params.as_ref() {
        header.extend_from_slice(&params.salt);
        header.extend_from_slice(&params.nonce);
        header.extend_from_slice(&params.m_cost.to_be_bytes());
        header.extend_from_slice(&params.t_cost.to_be_bytes());
        header.extend_from_slice(&params.p_cost.to_be_bytes());
    }
    let header_crc = crc32_bytes(&header);
    header.extend_from_slice(&header_crc.to_be_bytes());

    let total_len = header.len() as u64 + blob.len() as u64 + FOOTER_LEN as u64;
    let mut container = Vec::with_capacity(total_len as usize);
    container.extend_from_slice(&header);
    container.extend_from_slice(&blob);
    container.extend_from_slice(CONTAINER_FOOTER);
    container.extend_from_slice(&total_len.to_be_bytes());
    job.report(30.0, "Полезная нагрузка упакована.");
    Ok(container)
}
