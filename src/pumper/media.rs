//! Накачка медиа-контейнеров: MP3, FLAC и MP4/MOV.
//!
//! В отличие от «хвоста» из случайных байтов заполнение встаёт внутрь формата:
//! в MP3 — это padding внутри ID3v2-тега, во FLAC — блок PADDING, в MP4/MOV —
//! `free`-бокс. Плееры и редакторы такие файлы открывают как обычно, а объём
//! файла при этом ровно заданный.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::util::{random_bytes, Job, IO_CHUNK};

/// Какой медиа-контейнер найден по заголовку файла.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Mp3,
    Flac,
    Mp4,
}

impl MediaKind {
    pub fn title(self) -> &'static str {
        match self {
            MediaKind::Mp3 => "MP3",
            MediaKind::Flac => "FLAC",
            MediaKind::Mp4 => "MP4/MOV",
        }
    }
}

/// Максимальный размер тела ID3v2-тега: размер в теге — 28 бит.
const ID3_MAX: u64 = 0x0FFF_FFFF;
/// Максимальная длина полезной части блока FLAC: поле длины — 24 бита.
const FLAC_MAX_BLOCK: u64 = 0x00FF_FFFF;

/// Определяет медиа-контейнер по первым байтам файла.
pub fn detect(input: &Path) -> io::Result<Option<MediaKind>> {
    let mut file = File::open(input)?;
    let mut head = [0_u8; 12];
    let read = read_up_to(&mut file, &mut head)?;
    if read < 4 {
        return Ok(None);
    }
    if &head[..3] == b"ID3" {
        return Ok(Some(MediaKind::Mp3));
    }
    if &head[..4] == b"fLaC" {
        return Ok(Some(MediaKind::Flac));
    }
    if read >= 8 && &head[4..8] == b"ftyp" {
        return Ok(Some(MediaKind::Mp4));
    }
    // MP3 без тега начинается с фрейма: 11 единичных бит синхрослова.
    if head[0] == 0xFF && head[1] & 0xE0 == 0xE0 {
        return Ok(Some(MediaKind::Mp3));
    }
    Ok(None)
}

/// Добавляет заполнение в медиа-контейнер, попадая ровно в `target` байт.
pub fn run(
    kind: MediaKind,
    input: &Path,
    output: &Path,
    target: u64,
    job: &Job<'_>,
) -> Result<(), String> {
    job.check()?;
    let current = fs::metadata(input).map_err(|err| err.to_string())?.len();
    if target < current {
        return Err(format!(
            "Файл уже больше цели: {} байт, а нужно {target}.",
            crate::util::format_bytes(current as f64)
        ));
    }
    let result = match kind {
        MediaKind::Mp3 => mp3_padding(input, output, target, job),
        MediaKind::Flac => flac_padding(input, output, target, job),
        MediaKind::Mp4 => mp4_padding(input, output, target, job),
    };
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}

/// Читает не больше `buffer.len()` байтов (файл может быть короче).
fn read_up_to(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Копирует остаток входного файла в выходной, отчитываясь о прогрессе.
fn copy_tail(
    reader: &mut File,
    writer: &mut File,
    from: u64,
    job: &Job<'_>,
) -> Result<u64, String> {
    reader
        .seek(SeekFrom::Start(from))
        .map_err(|err| err.to_string())?;
    let mut buffer = vec![0_u8; IO_CHUNK.min(1024 * 1024)];
    let mut copied = 0_u64;
    loop {
        job.check()?;
        let read = reader.read(&mut buffer).map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|err| err.to_string())?;
        copied += read as u64;
        job.report(60.0, "Копирование данных...");
    }
    Ok(copied)
}

/// Пишет нули порциями, не выделяя большой буфер.
fn write_zeros(writer: &mut File, mut count: u64) -> Result<(), String> {
    let zeros = [0_u8; 64 * 1024];
    while count > 0 {
        let size = count.min(zeros.len() as u64) as usize;
        writer
            .write_all(&zeros[..size])
            .map_err(|err| err.to_string())?;
        count -= size as u64;
    }
    Ok(())
}

/// Пишет случайные байты порциями.
fn write_random(writer: &mut File, mut count: u64) -> Result<(), String> {
    while count > 0 {
        let size = count.min(IO_CHUNK as u64) as usize;
        let bytes = random_bytes(size)?;
        writer.write_all(&bytes).map_err(|err| err.to_string())?;
        count -= size as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------- MP3

/// Сколько байтов padding писать в каждый ID3v2-тег.
///
/// `body` — длина уже существующего тела тега (первый тег его переиспользует),
/// `needed` — сколько байтов нужно добавить к файлу.
fn id3_plan(needed: u64, body: u64, has_tag: bool) -> Result<Vec<u64>, String> {
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut plan = Vec::new();
    let mut left = needed;
    if has_tag {
        let cap = ID3_MAX.saturating_sub(body);
        let mut padding = left.min(cap);
        let mut rest = left - padding;
        if rest > 0 && rest < 10 {
            let take = (10 - rest).min(padding);
            padding -= take;
            rest += take;
        }
        plan.push(padding);
        left = rest;
    } else {
        if left < 10 {
            return Err("Для MP3 нужно добавить минимум 10 байт: добавляем ID3v2-тег.".to_owned());
        }
        left -= 10;
        let mut padding = left.min(ID3_MAX);
        let mut rest = left - padding;
        if rest > 0 && rest < 10 {
            let take = (10 - rest).min(padding);
            padding -= take;
            rest += take;
        }
        plan.push(padding);
        left = rest;
    }
    while left > 0 {
        if left < 10 {
            let last = plan.last_mut().expect("план не пуст");
            if *last + left <= ID3_MAX {
                *last += left;
                break;
            }
            return Err("Не удалось разложить заполнение по ID3v2-тегам.".to_owned());
        }
        left -= 10;
        let mut padding = left.min(ID3_MAX);
        let mut rest = left - padding;
        if rest > 0 && rest < 10 {
            let take = (10 - rest).min(padding);
            padding -= take;
            rest += take;
        }
        plan.push(padding);
        left = rest;
    }
    Ok(plan)
}

fn syncsafe(value: u64) -> [u8; 4] {
    [
        ((value >> 21) & 0x7F) as u8,
        ((value >> 14) & 0x7F) as u8,
        ((value >> 7) & 0x7F) as u8,
        (value & 0x7F) as u8,
    ]
}

fn mp3_padding(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let needed = target - current;
    let mut source = File::open(input).map_err(|err| err.to_string())?;
    let mut head = [0_u8; 10];
    let read = read_up_to(&mut source, &mut head).map_err(|err| err.to_string())?;
    let has_tag = read == 10 && &head[..3] == b"ID3";
    let mut body = Vec::new();
    let mut audio_start = 0_u64;
    if has_tag {
        let size = ((head[6] as u64) << 21)
            | ((head[7] as u64) << 14)
            | ((head[8] as u64) << 7)
            | head[9] as u64;
        body.resize(size as usize, 0);
        source
            .read_exact(&mut body)
            .map_err(|err| format!("ID3v2-тег обрезан: {err}"))?;
        audio_start = 10 + size;
    }
    let plan = id3_plan(needed, body.len() as u64, has_tag)?;
    job.report(5.0, "Добавление заполнения в ID3v2-тег...");
    let mut target_file = File::create(output).map_err(|err| err.to_string())?;
    for (index, padding) in plan.iter().enumerate() {
        let tag_body = if index == 0 { body.len() as u64 } else { 0 };
        let size = tag_body + padding;
        let mut header = [0_u8; 10];
        header[..6].copy_from_slice(b"ID3\x03\x00\x00");
        header[6..10].copy_from_slice(&syncsafe(size));
        target_file
            .write_all(&header)
            .map_err(|err| err.to_string())?;
        if index == 0 && has_tag {
            target_file
                .write_all(&body)
                .map_err(|err| err.to_string())?;
        }
        write_zeros(&mut target_file, *padding)?;
    }
    let copied = copy_tail(&mut source, &mut target_file, audio_start, job)?;
    target_file.flush().map_err(|err| err.to_string())?;
    drop(target_file);
    let len = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if len != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {len} (скопировано {copied})."
        ));
    }
    job.report(100.0, "Готово.");
    Ok(())
}

// ---------------------------------------------------------------- FLAC

/// Раскладывает добавку по блокам PADDING: работаем с блоками по 4 байта заголовка.
fn flac_plan(needed: u64, existing: Option<u64>) -> Result<(Option<u64>, Vec<u64>), String> {
    // (добавка к существующему PADDING, длины новых блоков)
    let mut extend = existing.map(|_| 0_u64);
    let mut left = needed;
    if let (Some(length), Some(extra)) = (existing, extend.as_mut()) {
        let add = left.min(FLAC_MAX_BLOCK.saturating_sub(length));
        *extra = add;
        left -= add;
    }
    let mut blocks = Vec::new();
    while left > 0 {
        if left < 4 {
            if let Some(extra) = extend.as_mut() {
                let length = existing.unwrap_or(0) + *extra;
                if length + left <= FLAC_MAX_BLOCK {
                    *extra += left;
                    break;
                }
            }
            return Err(
                "Для FLAC нужно добавить минимум 4 байта: добавляем блок PADDING.".to_owned(),
            );
        }
        left -= 4;
        let mut payload = left.min(FLAC_MAX_BLOCK);
        let mut rest = left - payload;
        if rest > 0 && rest < 4 {
            let take = (4 - rest).min(payload);
            payload -= take;
            rest += take;
        }
        blocks.push(payload);
        left = rest;
    }
    Ok((extend, blocks))
}

fn flac_padding(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let needed = target - current;
    let mut source = File::open(input).map_err(|err| err.to_string())?;
    let mut magic = [0_u8; 4];
    source
        .read_exact(&mut magic)
        .map_err(|err| err.to_string())?;
    if &magic != b"fLaC" {
        return Err("Это не FLAC-файл.".to_owned());
    }
    let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
    loop {
        let mut header = [0_u8; 4];
        source
            .read_exact(&mut header)
            .map_err(|err| format!("Блок FLAC обрезан: {err}"))?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let length =
            ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
        let mut data = vec![0_u8; length];
        source
            .read_exact(&mut data)
            .map_err(|err| format!("Блок FLAC обрезан: {err}"))?;
        blocks.push((kind, data));
        if last {
            break;
        }
    }
    let audio_start = source.stream_position().map_err(|err| err.to_string())?;
    let existing = blocks
        .iter()
        .position(|(kind, _)| *kind == 1)
        .map(|index| (index, blocks[index].1.len() as u64));
    let (extend, new_blocks) = flac_plan(needed, existing.map(|(_, length)| length))?;
    if let (Some((index, _)), Some(extra)) = (existing, extend) {
        let new_length = blocks[index].1.len() + extra as usize;
        blocks[index].1.resize(new_length, 0);
    }
    for payload in new_blocks {
        // Заполняем случайными байтами: архив такой файл не сожмёт.
        let data = random_bytes(payload as usize)?;
        blocks.push((1, data));
    }
    job.report(10.0, "Добавление блока PADDING во FLAC...");
    let mut target_file = File::create(output).map_err(|err| err.to_string())?;
    target_file
        .write_all(b"fLaC")
        .map_err(|err| err.to_string())?;
    let count = blocks.len();
    for (index, (kind, data)) in blocks.iter().enumerate() {
        let last = index + 1 == count;
        let length = data.len();
        if length > FLAC_MAX_BLOCK as usize {
            return Err("Блок FLAC слишком большой.".to_owned());
        }
        let header = [
            (if last { 0x80 } else { 0 }) | (kind & 0x7F),
            ((length >> 16) & 0xFF) as u8,
            ((length >> 8) & 0xFF) as u8,
            (length & 0xFF) as u8,
        ];
        target_file
            .write_all(&header)
            .map_err(|err| err.to_string())?;
        target_file.write_all(data).map_err(|err| err.to_string())?;
    }
    copy_tail(&mut source, &mut target_file, audio_start, job)?;
    target_file.flush().map_err(|err| err.to_string())?;
    drop(target_file);
    let len = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if len != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {len}."
        ));
    }
    job.report(100.0, "Готово.");
    Ok(())
}

// ---------------------------------------------------------------- MP4

fn mp4_padding(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    let current = fs::metadata(input).map_err(|err| err.to_string())?.len();
    let needed = target - current;
    if needed < 8 {
        return Err(
            "Для MP4/MOV нужно добавить минимум 8 байт: free-бокс не может быть меньше.".to_owned(),
        );
    }
    let mut source = File::open(input).map_err(|err| err.to_string())?;
    let mut head = [0_u8; 8];
    source
        .read_exact(&mut head)
        .map_err(|err| err.to_string())?;
    if &head[4..8] != b"ftyp" {
        return Err("Это не MP4/MOV-файл.".to_owned());
    }
    job.report(10.0, "Добавление free-бокса в MP4/MOV...");
    let mut target_file = File::create(output).map_err(|err| err.to_string())?;
    copy_tail(&mut source, &mut target_file, 0, job)?;
    if needed - 8 <= u32::MAX as u64 {
        let size = needed as u32;
        target_file
            .write_all(&size.to_be_bytes())
            .map_err(|err| err.to_string())?;
        target_file
            .write_all(b"free")
            .map_err(|err| err.to_string())?;
        write_random(&mut target_file, needed - 8)?;
    } else {
        target_file
            .write_all(&1_u32.to_be_bytes())
            .map_err(|err| err.to_string())?;
        target_file
            .write_all(b"free")
            .map_err(|err| err.to_string())?;
        target_file
            .write_all(&needed.to_be_bytes())
            .map_err(|err| err.to_string())?;
        write_random(&mut target_file, needed - 16)?;
    }
    target_file.flush().map_err(|err| err.to_string())?;
    drop(target_file);
    let len = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if len != target {
        return Err(format!(
            "Ожидался файл размером {target} байт, получилось {len}."
        ));
    }
    job.report(100.0, "Готово.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3_plan_covers_the_needed_bytes() {
        for needed in [1_u64, 9, 10, 11, 20, 100, 1024, ID3_MAX, ID3_MAX + 1] {
            for has_tag in [false, true] {
                for body in [0_u64, 100] {
                    if !has_tag && body > 0 {
                        continue;
                    }
                    if has_tag && body > needed {
                        continue;
                    }
                    let plan = match id3_plan(needed, body, has_tag) {
                        Ok(plan) => plan,
                        Err(_) => continue,
                    };
                    // Заголовки и padding должны дать ровно `needed` добавочных байт.
                    let mut total = 0_u64;
                    for (index, padding) in plan.iter().enumerate() {
                        total += 10;
                        if index == 0 && has_tag {
                            total -= 10; // заголовок уже был в файле
                        }
                        total += padding;
                    }
                    assert_eq!(
                        total, needed,
                        "needed={needed} has_tag={has_tag} body={body}"
                    );
                }
            }
        }
    }

    #[test]
    fn flac_plan_covers_the_needed_bytes() {
        for needed in [1_u64, 3, 4, 5, 8, 100, 1_000_000] {
            for existing in [None, Some(0_u64), Some(10)] {
                let (extend, blocks) = match flac_plan(needed, existing) {
                    Ok(plan) => plan,
                    Err(_) => continue,
                };
                let mut total = blocks.len() as u64 * 4 + blocks.iter().sum::<u64>();
                if let Some(extra) = extend {
                    total += extra;
                    // У существующего блока заголовок уже есть.
                }
                assert_eq!(total, needed, "needed={needed} existing={existing:?}");
            }
        }
    }

    #[test]
    fn mp3_gets_padding_inside_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("audio.mp3");
        // ID3v2.3 с пустым телом и пара «фреймов» после него.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ID3\x03\x00\x00\x00\x00\x00\x00");
        bytes.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64, 1, 2, 3, 4, 5]);
        fs::write(&input, &bytes).unwrap();
        let output = dir.path().join("pumped.mp3");
        let target = 64 * 1024;
        run(MediaKind::Mp3, &input, &output, target, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let result = fs::read(&output).unwrap();
        assert_eq!(&result[..3], b"ID3");
        let size = ((result[6] as u64) << 21)
            | ((result[7] as u64) << 14)
            | ((result[8] as u64) << 7)
            | result[9] as u64;
        assert_eq!(size as u64, target - 10 - 9);
        // Аудиоданные не тронуты и стоят в конце.
        assert!(result.ends_with(&[0xFF, 0xFB, 0x90, 0x64, 1, 2, 3, 4, 5]));
    }

    #[test]
    fn mp3_without_tag_gets_a_new_tag() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("plain.mp3");
        fs::write(&input, [0xFF, 0xFB, 0x90, 0x64, 7, 7, 7]).unwrap();
        let output = dir.path().join("pumped.mp3");
        let target = 4096;
        run(MediaKind::Mp3, &input, &output, target, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let result = fs::read(&output).unwrap();
        assert_eq!(&result[..3], b"ID3");
        assert!(result.ends_with(&[0xFF, 0xFB, 0x90, 0x64, 7, 7, 7]));
    }

    fn flac_sample(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        let count = blocks.len();
        for (index, (kind, data)) in blocks.iter().enumerate() {
            let last = index + 1 == count;
            let length = data.len();
            bytes.push((if last { 0x80 } else { 0 }) | (kind & 0x7F));
            bytes.push(((length >> 16) & 0xFF) as u8);
            bytes.push(((length >> 8) & 0xFF) as u8);
            bytes.push((length & 0xFF) as u8);
            bytes.extend_from_slice(data);
        }
        bytes.extend_from_slice(&[0xFF, 0xF8, 1, 2, 3]);
        bytes
    }

    fn read_flac_blocks(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut offset = 4;
        let mut blocks = Vec::new();
        loop {
            let kind = bytes[offset] & 0x7F;
            let last = bytes[offset] & 0x80 != 0;
            let length = ((bytes[offset + 1] as usize) << 16)
                | ((bytes[offset + 2] as usize) << 8)
                | bytes[offset + 3] as usize;
            let data = bytes[offset + 4..offset + 4 + length].to_vec();
            blocks.push((kind, data));
            offset += 4 + length;
            if last {
                break;
            }
        }
        blocks
    }

    #[test]
    fn flac_gets_a_padding_block_and_keeps_streaminfo() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("audio.flac");
        let source = flac_sample(&[(0, vec![1, 2, 3, 4]), (4, vec![9, 9])]);
        fs::write(&input, &source).unwrap();
        let output = dir.path().join("pumped.flac");
        let target = 32 * 1024;
        run(MediaKind::Flac, &input, &output, target, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let result = fs::read(&output).unwrap();
        let blocks = read_flac_blocks(&result);
        assert_eq!(blocks[0], (0, vec![1, 2, 3, 4]));
        assert!(blocks.iter().any(|(kind, _)| *kind == 1), "{blocks:?}");
        assert!(result.ends_with(&[0xFF, 0xF8, 1, 2, 3]));
    }

    #[test]
    fn mp4_gets_a_free_box() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clip.mp4");
        let mut source = Vec::new();
        source.extend_from_slice(&24_u32.to_be_bytes());
        source.extend_from_slice(b"ftypisom");
        source.extend_from_slice(&[0; 16]);
        source.extend_from_slice(&8_u32.to_be_bytes());
        source.extend_from_slice(b"free");
        fs::write(&input, &source).unwrap();
        let output = dir.path().join("pumped.mp4");
        let target = 16 * 1024;
        run(MediaKind::Mp4, &input, &output, target, &Job::quiet()).unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), target);
        let result = fs::read(&output).unwrap();
        assert_eq!(&result[..8], &source[..8]);
        // Последний бокс — free ровно до конца файла.
        let last = &result[source.len()..];
        let size = u32::from_be_bytes(last[..4].try_into().unwrap()) as u64;
        assert_eq!(&last[4..8], b"free");
        assert_eq!(size, last.len() as u64);
    }

    #[test]
    fn too_small_mp4_addition_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clip.mp4");
        let mut source = Vec::new();
        source.extend_from_slice(&24_u32.to_be_bytes());
        source.extend_from_slice(b"ftypisom");
        source.extend_from_slice(&[0; 16]);
        fs::write(&input, &source).unwrap();
        let output = dir.path().join("pumped.mp4");
        let error = run(
            MediaKind::Mp4,
            &input,
            &output,
            source.len() as u64 + 3,
            &Job::quiet(),
        )
        .unwrap_err();
        assert!(error.contains("минимум"), "{error}");
    }
}
