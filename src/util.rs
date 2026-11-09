//! Общие помощники: прогресс, отмена, случайные данные, контрольные суммы,
//! работа с путями и именами файлов.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const IO_CHUNK: usize = 1024 * 1024;
pub const CANCELLED_MESSAGE: &str = "Операция отменена пользователем.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordStrength {
    Weak,
    Medium,
    Strong,
}

impl PasswordStrength {
    pub fn label(self) -> &'static str {
        match self {
            PasswordStrength::Weak => "слабый",
            PasswordStrength::Medium => "средний",
            PasswordStrength::Strong => "сильный",
        }
    }
}

/// Грубая оценка пароля для подсказки в окне: длина и разнообразие классов
/// символов. Это не криптоанализ, а именно индикатор «стоит ли усложнить».
pub fn password_strength(value: &str) -> Option<PasswordStrength> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let length = value.chars().count();
    let mut classes = 0;
    if value.chars().any(|symbol| symbol.is_ascii_lowercase()) {
        classes += 1;
    }
    if value.chars().any(|symbol| symbol.is_ascii_uppercase()) {
        classes += 1;
    }
    if value.chars().any(|symbol| symbol.is_ascii_digit()) {
        classes += 1;
    }
    if value.chars().any(|symbol| !symbol.is_ascii_alphanumeric()) {
        classes += 1;
    }
    Some(if length >= 16 && classes >= 3 {
        PasswordStrength::Strong
    } else if length >= 10 && classes >= 2 {
        PasswordStrength::Medium
    } else {
        PasswordStrength::Weak
    })
}

/// Размер в удобных единицах: `512 КБ`, `10.0 МБ`, `1.5 ГБ`.
pub fn format_bytes(bytes: f64) -> String {
    if bytes >= GIB as f64 {
        format!("{:.1} ГБ", bytes / GIB as f64)
    } else if bytes >= MIB as f64 {
        format!("{:.1} МБ", bytes / MIB as f64)
    } else if bytes >= 1024.0 {
        format!("{:.1} КБ", bytes / 1024.0)
    } else {
        format!("{:.0} Б", bytes.max(0.0))
    }
}

pub fn format_speed(bytes_per_second: f64) -> String {
    format!("{}/с", format_bytes(bytes_per_second.max(0.0)))
}

/// Длительность в формате `MM:SS` или `HH:MM:SS`.
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

pub fn available_space(path: &Path) -> Option<u64> {
    let mut current = path;
    loop {
        if let Ok(free) = fs2::available_space(current) {
            return Some(free);
        }
        current = current.parent()?;
    }
}

/// Предупреждение, если на диске для результата не хватает места.
pub fn disk_space_warning(path: &Path, required: u64) -> Option<String> {
    let free = available_space(path)?;
    (free < required).then(|| {
        format!(
            "На диске может не хватить места: нужно {}, свободно {} ({}).",
            format_bytes(required as f64),
            format_bytes(free as f64),
            path.display()
        )
    })
}

/// Флаг отмены, разделяемый интерфейсом и рабочим потоком.
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Контекст длительной операции: сообщения о прогрессе и проверка отмены.
pub struct Job<'a> {
    progress: &'a dyn Fn(f32, &str),
    cancel: Option<&'a CancelToken>,
}

fn silent_progress(_: f32, _: &str) {}

impl<'a> Job<'a> {
    pub fn new(progress: &'a dyn Fn(f32, &str), cancel: &'a CancelToken) -> Self {
        Self {
            progress,
            cancel: Some(cancel),
        }
    }

    /// Операция без прогресса и без отмены — удобно в тестах.
    pub fn quiet() -> Job<'static> {
        Job {
            progress: &silent_progress,
            cancel: None,
        }
    }

    /// Дочерний контекст: тот же флаг отмены, но собственный обработчик прогресса.
    /// Нужен пакетной обработке: каждый файл рапортует о себе, отмена общая.
    pub fn child<'b>(&'b self, progress: &'b dyn Fn(f32, &str)) -> Job<'b>
    where
        'a: 'b,
    {
        Job {
            progress,
            cancel: self.cancel,
        }
    }

    pub fn report(&self, percent: f32, text: &str) {
        (self.progress)(percent.clamp(0.0, 100.0), text);
    }

    pub fn check(&self) -> Result<(), String> {
        match self.cancel {
            Some(token) if token.is_cancelled() => Err(CANCELLED_MESSAGE.to_owned()),
            _ => Ok(()),
        }
    }
}

pub fn random_bytes(size: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0_u8; size];
    getrandom::getrandom(&mut bytes)
        .map_err(|err| format!("Ошибка генерации случайных данных: {err}"))?;
    Ok(bytes)
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

pub fn write_random_stream<W: Write>(
    writer: &mut W,
    total: u64,
    job: &Job<'_>,
    message: &str,
) -> Result<(), String> {
    let mut written = 0_u64;
    while written < total {
        job.check()?;
        let size = (total - written).min(IO_CHUNK as u64) as usize;
        let bytes = random_bytes(size)?;
        writer.write_all(&bytes).map_err(|err| err.to_string())?;
        written += size as u64;
        job.report(
            written as f32 / total.max(1) as f32 * 100.0,
            &format!("{message}: {:.1} МБ", written as f64 / MIB as f64),
        );
    }
    Ok(())
}

pub fn crc32_bytes(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

pub fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

pub fn read_u64_be(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset + 8)?;
    let mut value = [0_u8; 8];
    value.copy_from_slice(slice);
    Some(u64::from_be_bytes(value))
}

/// Приводит путь к сравнимому виду, даже если файл ещё не создан.
fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match (parent.canonicalize(), path.file_name()) {
        (Ok(directory), Some(name)) => directory.join(name),
        (Ok(directory), None) => directory,
        _ => path.to_path_buf(),
    }
}

/// Сравнивает пути так, чтобы не перезаписать исходный файл на регистронезависимой
/// файловой системе и при ещё не созданном целевом файле.
pub fn same_file(first: &Path, second: &Path) -> bool {
    normalize_path(first) == normalize_path(second)
}

/// Подбирает свободное имя: при совпадении добавляет `-2`, `-3`, … перед расширением.
/// Используется пакетной обработкой, когда результаты складываются в одну папку.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    for index in 2..10_000_u32 {
        let candidate = suffixed_path(path, index);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// `photo.png` + 3 → `photo-3.png`.
fn suffixed_path(path: &Path, index: u32) -> PathBuf {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file")
        .to_owned();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_owned);
    let name = match extension.as_deref() {
        Some(extension) => format!("{stem}-{index}.{extension}"),
        None => format!("{stem}-{index}"),
    };
    if parent.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        parent.join(name)
    }
}

/// Набор уже занятых имён: нужен пакетной обработке, чтобы два файла с одинаковым
/// именем из разных папок не получили один и тот же результат — в том числе когда
/// файлы обрабатываются параллельно и проверка `exists()` ещё ничего не видит.
#[derive(Default)]
pub struct Reservations {
    taken: HashSet<PathBuf>,
}

impl Reservations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает свободный путь и запоминает его как занятый.
    pub fn reserve(&mut self, path: &Path) -> PathBuf {
        let candidate = if self.is_taken(path) {
            (2..10_000_u32)
                .map(|index| suffixed_path(path, index))
                .find(|candidate| !self.is_taken(candidate) && !candidate.exists())
                .unwrap_or_else(|| path.to_path_buf())
        } else {
            path.to_path_buf()
        };
        self.taken.insert(normalize_path(&candidate));
        candidate
    }

    pub fn is_taken(&self, path: &Path) -> bool {
        self.taken.contains(&normalize_path(path)) || path.exists()
    }
}

const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.trim().to_ascii_uppercase();
    RESERVED_NAMES.contains(&upper.as_str()) || upper == "CONIN$" || upper == "CONOUT$"
}

fn truncate_name(name: &str) -> String {
    let limit = 120;
    if name.chars().count() <= limit {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if extension.chars().count() <= 16 => (stem, Some(extension)),
        _ => (name, None),
    };
    let mut shortened: String = stem.chars().take(limit.saturating_sub(20)).collect();
    shortened.push('~');
    if let Some(extension) = extension {
        shortened.push('.');
        shortened.push_str(extension);
    }
    shortened
}

/// Чистит имя из контейнера: убирает разделители, управляющие символы,
/// зарезервированные имена Windows и слишком длинные варианты.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name
        .rsplit(['\\', '/'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|symbol| {
            !matches!(symbol, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                && !symbol.is_control()
        })
        .collect();
    let trimmed = cleaned.trim_end_matches(['.', ' ']).trim();
    if trimmed.is_empty() {
        return "payload.bin".to_owned();
    }
    let candidate = truncate_name(trimmed);
    if is_reserved(&candidate) {
        format!("_{candidate}")
    } else {
        candidate
    }
}

/// Открывает проводник с выделением файла (на других платформах — каталог).
pub fn reveal_in_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let argument = format!("/select,{}", path.display());
        let _ = std::process::Command::new("explorer").arg(argument).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let _ = std::process::Command::new("xdg-open")
            .arg(directory)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encoding_is_lowercase_and_preserves_leading_zeroes() {
        assert_eq!(hex_encode(&[0, 15, 171, 255]), "000fabff");
    }

    #[test]
    fn sanitizer_removes_traversal_and_reserved_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(
            sanitize_file_name("C:\\Windows\\system32\\cmd.exe"),
            "cmd.exe"
        );
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("LPT9"), "_LPT9");
        assert_eq!(sanitize_file_name(".."), "payload.bin");
        assert_eq!(sanitize_file_name("   "), "payload.bin");
        assert_eq!(sanitize_file_name("report.."), "report");
        assert!(sanitize_file_name(&"a".repeat(500)).chars().count() <= 120);
    }

    #[test]
    fn same_file_compares_existing_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("carrier.png");
        std::fs::write(&existing, b"x").unwrap();
        let missing = dir.path().join("other.png");
        assert!(!same_file(&existing, &missing));
        assert!(same_file(&existing, &existing));
        let missing_same = dir.path().join("carrier.png");
        assert!(same_file(&existing, &missing_same));
    }

    #[test]
    fn password_strength_grows_with_length_and_variety() {
        assert_eq!(password_strength(""), None);
        assert_eq!(password_strength("123456"), Some(PasswordStrength::Weak));
        assert_eq!(
            password_strength("correct-horse"),
            Some(PasswordStrength::Medium)
        );
        assert_eq!(
            password_strength("Correct-Horse-42!"),
            Some(PasswordStrength::Strong)
        );
    }

    #[test]
    fn human_units_and_durations_are_readable() {
        assert_eq!(format_bytes(512.0), "512 Б");
        assert_eq!(format_bytes(1536.0), "1.5 КБ");
        assert_eq!(format_bytes(MIB as f64), "1.0 МБ");
        assert_eq!(format_speed(MIB as f64), "1.0 МБ/с");
        assert_eq!(format_duration(9.4), "00:09");
        assert_eq!(format_duration(65.0), "01:05");
        assert_eq!(format_duration(3661.0), "01:01:01");
    }

    #[test]
    fn cancellation_is_reported_as_error() {
        let token = CancelToken::new();
        let progress = |_: f32, _: &str| {};
        let job = Job::new(&progress, &token);
        assert!(job.check().is_ok());
        token.cancel();
        assert_eq!(job.check().unwrap_err(), CANCELLED_MESSAGE);
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }
}
