//! Групповые операции с полезной нагрузкой.
//!
//! Здесь живут три вещи, которых не хватало поштучным командам:
//! пары «носитель ← что спрятать» с отдельным паролем на пару, обзор сразу
//! нескольких файлов (что где спрятано) и извлечение всего найденного в папку.
//!
//! Имена извлечённых файлов берутся из контейнера вместе с расширением: именно
//! расширение подсказывает системе, чем открывать файл, поэтому терять его нельзя.

use std::fs;
use std::path::{Path, PathBuf};

use super::format::PAYLOAD_NOT_FOUND;
use super::{
    extract_bundle_to, extract_to, inspect_file, ContainerOptions, ExtractOutcome, PayloadSummary,
};
use crate::util::{sanitize_file_name, unique_path, Job};

/// Имя файла для отчёта: без папок, но с расширением.
fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Пара группового встраивания: носитель, что в него спрятать и пароль пары.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedPair {
    pub carrier: PathBuf,
    /// Один файл — обычное встраивание, несколько — ZIP-бандл внутри контейнера.
    pub payloads: Vec<PathBuf>,
    /// Пароль пары; `None` — использовать общий пароль из настроек.
    pub password: Option<String>,
}

impl EmbedPair {
    /// Новый файл с одной нагрузкой и без своего пароля.
    pub fn single(carrier: PathBuf, payload: PathBuf) -> Self {
        Self {
            carrier,
            payloads: vec![payload],
            password: None,
        }
    }

    pub fn bundle(&self) -> bool {
        self.payloads.len() > 1
    }

    /// `photo.png ← song.mp3` или `photo.png ← 2 файла (бандл)`.
    pub fn label(&self) -> String {
        let carrier = file_label(&self.carrier);
        match self.payloads.len() {
            0 => format!("{carrier} ← пусто"),
            1 => format!("{carrier} ← {}", file_label(&self.payloads[0])),
            count => format!("{carrier} ← {count} файла (бандл)"),
        }
    }

    /// Настройки контейнера: пароль пары важнее общего.
    pub fn options(&self, base: &ContainerOptions) -> ContainerOptions {
        let mut options = base.clone();
        if self.password.is_some() {
            options.password = self.password.clone();
        }
        options
    }
}

/// Пароль пары по её номеру: если для пары пароль не задан, берётся последний
/// указанный — так `--pair-password` можно задать один раз на все пары.
fn pair_password(passwords: &[Option<String>], index: usize) -> Option<String> {
    match passwords.get(index) {
        Some(value) => value.clone(),
        None => passwords.last().cloned().flatten(),
    }
}

/// Как сопоставлять носители и нагрузки при групповом встраивании.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PairMode {
    /// Первый носитель получает первую нагрузку, второй — вторую и так далее.
    #[default]
    Order,
    /// Носитель и нагрузка совпадают по основе имени: `photo.png` ↔ `photo.zip`.
    Name,
}

impl PairMode {
    pub const ALL: [PairMode; 2] = [PairMode::Order, PairMode::Name];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_lowercase().as_str() {
            "order" | "по-порядку" | "порядок" => Ok(PairMode::Order),
            "name" | "names" | "по-именам" | "имя" => Ok(PairMode::Name),
            other => Err(format!(
                "Неизвестный способ сопоставления пар: {other}. Допустимо: order, name."
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PairMode::Order => "по порядку",
            PairMode::Name => "по именам",
        }
    }
}

/// Основа имени для сопоставления: `photo.png` и `Photo.zip` дают `photo`.
fn match_key(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().trim().to_lowercase())
        .filter(|stem| !stem.is_empty())
}

/// Делит нагрузки между носителями: поровну и без пустых групп.
///
/// Нагрузок не больше, чем носителей — каждая идёт в свой носитель, лишние
/// носители получают нагрузку по кругу. Нагрузок больше — они распределяются
/// поровну, а носитель с несколькими файлами получает бандл.
fn split_payloads(payloads: &[PathBuf], carriers: usize) -> Vec<Vec<PathBuf>> {
    let carriers = carriers.max(1);
    if payloads.len() <= carriers {
        return (0..carriers)
            .map(|index| vec![payloads[index % payloads.len()].clone()])
            .collect();
    }
    let base = payloads.len() / carriers;
    let extra = payloads.len() % carriers;
    let mut groups = Vec::with_capacity(carriers);
    let mut start = 0_usize;
    for index in 0..carriers {
        let size = base + usize::from(index < extra);
        groups.push(payloads[start..start + size].to_vec());
        start += size;
    }
    groups
}

/// Сопоставляет носители и нагрузки по порядку.
///
/// Порядок важен: первый носитель получает первую нагрузку и так далее.
pub fn pair_by_order(
    carriers: &[PathBuf],
    payloads: &[PathBuf],
    passwords: &[Option<String>],
) -> Result<Vec<EmbedPair>, String> {
    if carriers.is_empty() {
        return Err("Не указан ни один носитель.".to_owned());
    }
    if payloads.is_empty() {
        return Err("Не указан ни один файл нагрузки.".to_owned());
    }
    let groups = split_payloads(payloads, carriers.len());
    Ok(carriers
        .iter()
        .enumerate()
        .map(|(index, carrier)| EmbedPair {
            carrier: carrier.clone(),
            payloads: groups[index].clone(),
            password: pair_password(passwords, index),
        })
        .collect())
}

/// Сопоставляет носители и нагрузки по основе имени.
///
/// `photo.png` получает `photo.zip` и `photo.mp3` (несколько файлов — бандл),
/// `report.pdf` — `report.txt`. Регистр не важен. Если хотя бы одному файлу
/// не нашлось пары, возвращается ошибка со списком таких файлов — лучше
/// показать её сразу, чем молча разложить данные не туда.
pub fn pair_by_name(
    carriers: &[PathBuf],
    payloads: &[PathBuf],
    passwords: &[Option<String>],
) -> Result<Vec<EmbedPair>, String> {
    if carriers.is_empty() {
        return Err("Не указан ни один носитель.".to_owned());
    }
    if payloads.is_empty() {
        return Err("Не указан ни один файл нагрузки.".to_owned());
    }
    let mut remaining: std::collections::HashMap<String, Vec<PathBuf>> =
        std::collections::HashMap::new();
    for payload in payloads {
        let key = match_key(payload).ok_or_else(|| {
            format!(
                "У файла нагрузки нет имени для сопоставления: {}",
                payload.display()
            )
        })?;
        remaining.entry(key).or_default().push(payload.clone());
    }

    let mut pairs = Vec::with_capacity(carriers.len());
    let mut missing = Vec::new();
    for (index, carrier) in carriers.iter().enumerate() {
        let Some(key) = match_key(carrier) else {
            missing.push(format!("{} (нет имени)", carrier.display()));
            continue;
        };
        match remaining.remove(&key) {
            Some(group) => pairs.push(EmbedPair {
                carrier: carrier.clone(),
                payloads: group,
                password: pair_password(passwords, index),
            }),
            None => missing.push(format!("{} (нет нагрузки «{key}»)", carrier.display())),
        }
    }
    if !missing.is_empty() {
        let mut details = vec![format!("не найдены пары для: {}", missing.join(", "))];
        if !remaining.is_empty() {
            let mut leftovers: Vec<String> = remaining
                .into_values()
                .flatten()
                .map(|path| file_label(&path))
                .collect();
            leftovers.sort();
            details.push(format!("лишние нагрузки: {}", leftovers.join(", ")));
        }
        return Err(format!(
            "Не удалось сопоставить по именам: {}.",
            details.join("; ")
        ));
    }
    if !remaining.is_empty() {
        let mut leftovers: Vec<String> = remaining
            .into_values()
            .flatten()
            .map(|path| file_label(&path))
            .collect();
        leftovers.sort();
        return Err(format!(
            "Эти файлы нагрузки остались без носителя: {}.",
            leftovers.join(", ")
        ));
    }
    Ok(pairs)
}

/// Что нашлось в одном файле при обзоре группы.
#[derive(Clone, Debug)]
pub enum ScanResult {
    /// Полезная нагрузка есть.
    Found(PayloadSummary),
    /// Файл обычный: внутри ничего не спрятано.
    Empty,
    /// Файл не читается или контейнер повреждён.
    Error(String),
}

/// Строка обзора: файл и то, что в нём нашлось.
#[derive(Clone, Debug)]
pub struct ScanItem {
    pub path: PathBuf,
    pub result: ScanResult,
}

impl ScanItem {
    pub fn is_found(&self) -> bool {
        matches!(self.result, ScanResult::Found(_))
    }

    pub fn summary(&self) -> Option<&PayloadSummary> {
        match &self.result {
            ScanResult::Found(summary) => Some(summary),
            _ => None,
        }
    }

    /// Одна строка для отчёта: `photo.png — song.mp3 (48.8 КБ, пароль)`.
    pub fn describe(&self) -> String {
        let name = file_label(&self.path);
        match &self.result {
            ScanResult::Found(summary) => {
                let info = &summary.info;
                let mut details = vec![format!("{:.1} КБ", info.size as f64 / 1024.0)];
                if info.encrypted {
                    details.push("пароль".to_owned());
                }
                if info.compressed {
                    details.push("сжатие".to_owned());
                }
                if info.bundle {
                    details.push("бандл".to_owned());
                }
                if info.copies > 1 {
                    details.push(format!("копий: {}", info.copies));
                }
                let what = if info.bundle {
                    "несколько файлов".to_owned()
                } else {
                    info.name.clone()
                };
                format!("{name} — {what} ({})", details.join(", "))
            }
            ScanResult::Empty => format!("{name} — ничего не спрятано"),
            ScanResult::Error(error) => format!("{name} — ошибка: {error}"),
        }
    }
}

/// Итог по списку обзора: `с нагрузкой 2 из 3`.
pub fn scan_summary(items: &[ScanItem]) -> String {
    let found = items.iter().filter(|item| item.is_found()).count();
    let empty = items
        .iter()
        .filter(|item| matches!(item.result, ScanResult::Empty))
        .count();
    let failed = items
        .iter()
        .filter(|item| matches!(item.result, ScanResult::Error(_)))
        .count();
    let mut parts = vec![format!("с нагрузкой {found} из {}", items.len())];
    if empty > 0 {
        parts.push(format!("без нагрузки {empty}"));
    }
    if failed > 0 {
        parts.push(format!("ошибок {failed}"));
    }
    parts.join(", ")
}

/// Проверяет один файл, перебирая пароли: сначала без пароля, затем по одному
/// из списка. Возвращает сводку и пароль, который подошёл.
///
/// Перебор нужен группам: файлы одной пачки могли спрятать с разными паролями,
/// и пользователю не приходится вспоминать, какой пароль к какому файлу.
pub fn probe(
    carrier: &Path,
    passwords: &[String],
) -> Result<(PayloadSummary, Option<String>), String> {
    let candidates: Vec<&str> = passwords
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    let first_error = match inspect_file(carrier, None) {
        Ok(summary) if !summary.info.encrypted => return Ok((summary, None)),
        Ok(_) => "Полезная нагрузка зашифрована: нужен пароль.".to_owned(),
        Err(error) if error == PAYLOAD_NOT_FOUND => return Err(error),
        Err(error) => error,
    };
    for candidate in candidates {
        if let Ok(summary) = inspect_file(carrier, Some(candidate)) {
            return Ok((summary, Some(candidate.to_owned())));
        }
    }
    Err(first_error)
}

/// Проверяет список файлов: в каком что спрятано.
///
/// Обычные файлы не считаются ошибкой — про них просто сообщается, что внутри
/// ничего нет. Ошибка означает, что файл не прочитался или контейнер повреждён.
pub fn scan_many(paths: &[PathBuf], passwords: &[String], job: &Job<'_>) -> Vec<ScanItem> {
    let total = paths.len().max(1) as f32;
    let mut items = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        if job.check().is_err() {
            break;
        }
        let local = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            job.report(overall, text);
        };
        let item_job = job.child(&local);
        item_job.report(0.0, &format!("Проверка {}...", file_label(path)));
        let result = match probe(path, passwords) {
            Ok((summary, _)) => ScanResult::Found(summary),
            Err(error) if error == PAYLOAD_NOT_FOUND => ScanResult::Empty,
            Err(error) => ScanResult::Error(error),
        };
        items.push(ScanItem {
            path: path.clone(),
            result,
        });
    }
    items
}

/// Итог извлечения из одного носителя.
#[derive(Clone, Debug)]
pub enum ExtractResult {
    /// Один файл извлечён: путь, имя и размер.
    File(ExtractOutcome),
    /// Бандл распакован в подпапку.
    Bundle(Vec<(PathBuf, u64)>),
    /// Нагрузки в файле нет.
    Empty,
    /// Файл не прочитался, пароль не подошёл или контейнер повреждён.
    Error(String),
}

/// Строка группового извлечения.
#[derive(Clone, Debug)]
pub struct ExtractItem {
    pub carrier: PathBuf,
    pub result: ExtractResult,
}

impl ExtractItem {
    /// Что-то действительно записано на диск.
    pub fn is_done(&self) -> bool {
        matches!(
            self.result,
            ExtractResult::File(_) | ExtractResult::Bundle(_)
        )
    }

    /// Одна строка для отчёта: `photo.png → song.mp3`.
    pub fn describe(&self) -> String {
        let name = file_label(&self.carrier);
        match &self.result {
            ExtractResult::File(outcome) => format!(
                "{name} → {} ({:.1} КБ)",
                outcome.output.display(),
                outcome.size as f64 / 1024.0
            ),
            ExtractResult::Bundle(files) => {
                let folder = files
                    .first()
                    .and_then(|(path, _)| path.parent())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "папка".to_owned());
                format!("{name} → бандл, {} файл(ов) в {folder}", files.len())
            }
            ExtractResult::Empty => format!("{name} — ничего не спрятано"),
            ExtractResult::Error(error) => format!("{name} — ошибка: {error}"),
        }
    }
}

/// Итог по списку извлечения: `извлечено 2 из 3, без нагрузки 1`.
pub fn extract_summary(items: &[ExtractItem]) -> String {
    let done = items.iter().filter(|item| item.is_done()).count();
    let empty = items
        .iter()
        .filter(|item| matches!(item.result, ExtractResult::Empty))
        .count();
    let failed = items
        .iter()
        .filter(|item| matches!(item.result, ExtractResult::Error(_)))
        .count();
    let mut parts = vec![format!("извлечено {done} из {}", items.len())];
    if empty > 0 {
        parts.push(format!("без нагрузки {empty}"));
    }
    if failed > 0 {
        parts.push(format!("ошибок {failed}"));
    }
    parts.join(", ")
}

/// Итог группового извлечения и место, куда всё сложено.
#[derive(Clone, Debug)]
pub struct ExtractBatch {
    /// Папка результатов; `None` — файлы легли рядом со своими носителями.
    pub out_dir: Option<PathBuf>,
    pub items: Vec<ExtractItem>,
}

impl ExtractBatch {
    pub fn summary(&self) -> String {
        extract_summary(&self.items)
    }

    /// Текст отчёта: итог, место и строка на каждый носитель.
    pub fn report(&self) -> String {
        let folder = match self.out_dir.as_ref() {
            Some(directory) => directory.display().to_string(),
            None => "рядом с носителями".to_owned(),
        };
        let mut text = format!("{}\nПапка: {folder}", self.summary());
        for item in &self.items {
            text.push_str("\n  ");
            text.push_str(&item.describe());
        }
        text
    }
}

/// Папка для бандла из носителя: `photo.png` → `photo`.
fn bundle_folder(carrier: &Path) -> String {
    let stem = carrier
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("bundle");
    let safe = sanitize_file_name(stem);
    if safe.is_empty() || safe == "payload.bin" {
        "bundle".to_owned()
    } else {
        safe
    }
}

/// Извлекает всё, что нашлось, в одну папку.
///
/// Одиночные файлы сохраняются под именем из контейнера — с расширением, чтобы
/// система понимала, чем открывать файл. Бандл распаковывается в подпапку с
/// именем носителя, поэтому одинаковые имена из разных файлов не конфликтуют.
/// `out_dir` = `None` — каждый результат ложится рядом со своим носителем.
pub fn extract_many(
    carriers: &[PathBuf],
    out_dir: Option<&Path>,
    passwords: &[String],
    job: &Job<'_>,
) -> Result<ExtractBatch, String> {
    if let Some(directory) = out_dir {
        fs::create_dir_all(directory)
            .map_err(|err| format!("Не удалось создать папку {}: {err}", directory.display()))?;
    }
    let total = carriers.len().max(1) as f32;
    // Один носитель — бандл ложится прямо в выбранную папку. Несколько — каждому
    // своя подпапка, иначе файлы из разных бандлов перемешаются.
    let nested = carriers.len() > 1;
    let mut items = Vec::with_capacity(carriers.len());
    for (index, carrier) in carriers.iter().enumerate() {
        job.check()?;
        let local = |percent: f32, text: &str| {
            let overall = (index as f32 + percent.clamp(0.0, 100.0) / 100.0) / total * 100.0;
            job.report(overall, text);
        };
        let item_job = job.child(&local);
        let result = match probe(carrier, passwords) {
            Ok((summary, password)) if summary.info.bundle => {
                let base = match out_dir {
                    Some(directory) => directory.to_path_buf(),
                    None => carrier
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from(".")),
                };
                let folder = if nested {
                    unique_path(&base.join(bundle_folder(carrier)))
                } else {
                    base
                };
                extract_bundle_to(carrier, &folder, password.as_deref(), &item_job)
                    .map(ExtractResult::Bundle)
            }
            Ok((summary, password)) => {
                let name = sanitize_file_name(&summary.info.name);
                let target = match out_dir {
                    Some(directory) => unique_path(&directory.join(&name)),
                    None => unique_path(&super::default_extract_path(carrier, &name)),
                };
                extract_to(carrier, &target, password.as_deref(), &item_job)
                    .map(ExtractResult::File)
            }
            Err(error) if error == PAYLOAD_NOT_FOUND => Ok(ExtractResult::Empty),
            Err(error) => Err(error),
        };
        items.push(ExtractItem {
            carrier: carrier.clone(),
            result: match result {
                Ok(value) => value,
                Err(error) => ExtractResult::Error(error),
            },
        });
    }
    Ok(ExtractBatch {
        out_dir: out_dir.map(Path::to_path_buf),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn png_with_payload(dir: &Path, name: &str, payload_name: &str, data: &[u8]) -> PathBuf {
        let carrier = dir.join(name);
        image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 8, y as u8 * 8, 40]))
            .save(&carrier)
            .unwrap();
        let payload = dir.join(payload_name);
        fs::write(&payload, data).unwrap();
        let output = dir.join(format!("hidden-{name}"));
        super::super::embed_file(
            &carrier,
            &payload,
            &output,
            &ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();
        output
    }

    #[test]
    fn payloads_and_carriers_pair_up_in_order() {
        let carriers = paths(&["a.png", "b.png"]);
        let payloads = paths(&["one.txt", "two.txt"]);
        let pairs = pair_by_order(&carriers, &payloads, &[]).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].payloads, paths(&["one.txt"]));
        assert_eq!(pairs[1].payloads, paths(&["two.txt"]));
        assert!(!pairs[0].bundle());
        assert_eq!(pairs[0].label(), "a.png ← one.txt");
    }

    #[test]
    fn one_payload_goes_to_every_carrier() {
        let pairs =
            pair_by_order(&paths(&["a.png", "b.png"]), &paths(&["secret.pdf"]), &[]).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs
            .iter()
            .all(|pair| pair.payloads == paths(&["secret.pdf"])));
    }

    #[test]
    fn pairs_by_name_match_stems_and_ignore_case() {
        let pairs = pair_by_name(
            &paths(&["Photo.PNG", "report.pdf"]),
            &paths(&["photo.zip", "REPORT.txt"]),
            &[],
        )
        .unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].payloads, paths(&["photo.zip"]));
        assert_eq!(pairs[1].payloads, paths(&["REPORT.txt"]));
    }

    #[test]
    fn pairs_by_name_bundle_equal_stems() {
        let pairs = pair_by_name(
            &paths(&["photo.png"]),
            &paths(&["photo.zip", "photo.mp3"]),
            &[],
        )
        .unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].bundle());
        assert_eq!(pairs[0].payloads.len(), 2);
    }

    #[test]
    fn pairs_by_name_report_missing_matches() {
        let error = pair_by_name(
            &paths(&["photo.png", "report.pdf"]),
            &paths(&["photo.zip", "other.mp3"]),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("report.pdf"), "{error}");
        assert!(error.contains("other.mp3"), "{error}");
    }

    #[test]
    fn pair_mode_parses_russian_and_english_names() {
        assert_eq!(PairMode::parse("order").unwrap(), PairMode::Order);
        assert_eq!(PairMode::parse("по-именам").unwrap(), PairMode::Name);
        assert!(PairMode::parse("что-то").is_err());
    }

    #[test]
    fn extra_payloads_become_bundles_split_evenly() {
        let pairs = pair_by_order(
            &paths(&["a.png", "b.png"]),
            &paths(&["1.txt", "2.txt", "3.txt", "4.txt", "5.txt"]),
            &[],
        )
        .unwrap();
        assert_eq!(pairs[0].payloads.len(), 3);
        assert_eq!(pairs[1].payloads.len(), 2);
        assert!(pairs.iter().all(EmbedPair::bundle));
        assert_eq!(pairs[0].label(), "a.png ← 3 файла (бандл)");
    }

    #[test]
    fn pair_passwords_follow_order_and_repeat_the_last() {
        let carriers = paths(&["a.png", "b.png", "c.png"]);
        let payloads = paths(&["one.txt", "two.txt", "three.txt"]);
        let passwords = vec![Some("first".to_owned()), None, Some("third".to_owned())];
        let pairs = pair_by_order(&carriers, &payloads, &passwords).unwrap();
        assert_eq!(pairs[0].password.as_deref(), Some("first"));
        assert_eq!(pairs[1].password, None);
        assert_eq!(pairs[2].password.as_deref(), Some("third"));

        let short = vec![Some("only".to_owned())];
        let pairs = pair_by_order(&carriers, &payloads, &short).unwrap();
        assert!(pairs
            .iter()
            .all(|pair| pair.password.as_deref() == Some("only")));
    }

    #[test]
    fn pair_options_override_only_the_password() {
        let base = ContainerOptions {
            compress: true,
            password: Some("shared".to_owned()),
            bundle: false,
        };
        let mut pair = EmbedPair::single(PathBuf::from("a.png"), PathBuf::from("one.txt"));
        assert_eq!(pair.options(&base).password.as_deref(), Some("shared"));
        assert!(pair.options(&base).compress);
        pair.password = Some("own".to_owned());
        assert_eq!(pair.options(&base).password.as_deref(), Some("own"));
        assert!(pair.options(&base).compress);
    }

    #[test]
    fn pairing_requires_both_sides() {
        assert!(pair_by_order(&[], &paths(&["one.txt"]), &[]).is_err());
        assert!(pair_by_order(&paths(&["a.png"]), &[], &[]).is_err());
    }

    #[test]
    fn scan_reports_which_files_hold_a_payload() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = png_with_payload(dir.path(), "one.png", "song.mp3", b"secret bytes");
        let plain = dir.path().join("plain.txt");
        fs::write(&plain, b"just a normal file").unwrap();

        let items = scan_many(&[hidden.clone(), plain.clone()], &[], &Job::quiet());
        assert_eq!(items.len(), 2);
        assert!(items[0].is_found());
        assert_eq!(items[0].summary().unwrap().info.name, "song.mp3");
        assert!(matches!(items[1].result, ScanResult::Empty));
        assert_eq!(scan_summary(&items), "с нагрузкой 1 из 2, без нагрузки 1");
        assert!(items[0].describe().contains("song.mp3"));
        assert!(items[1].describe().contains("ничего не спрятано"));
    }

    #[test]
    fn extract_keeps_names_and_skips_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = png_with_payload(dir.path(), "one.png", "song.mp3", b"secret bytes");
        let plain = dir.path().join("plain.txt");
        fs::write(&plain, b"just a normal file").unwrap();
        let out_dir = dir.path().join("restored");

        let batch = extract_many(&[hidden, plain], Some(&out_dir), &[], &Job::quiet()).unwrap();
        assert_eq!(batch.items.len(), 2);
        assert!(batch.items[0].is_done());
        assert!(matches!(batch.items[1].result, ExtractResult::Empty));
        assert!(
            out_dir.join("song.mp3").is_file(),
            "имя с расширением сохранено"
        );
        assert_eq!(fs::read(out_dir.join("song.mp3")).unwrap(), b"secret bytes");
        assert!(batch.report().contains("извлечено 1 из 2, без нагрузки 1"));
    }

    #[test]
    fn bundle_is_unpacked_into_the_target_folder() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("photo.png");
        image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8, y as u8, 7]))
            .save(&carrier)
            .unwrap();
        let first = dir.path().join("note.txt");
        let second = dir.path().join("song.mp3");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let output = dir.path().join("hidden.png");
        super::super::embed_bundle(
            &carrier,
            &[first, second],
            &output,
            &ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();

        let out_dir = dir.path().join("restored");
        let batch = extract_many(&[output], Some(&out_dir), &[], &Job::quiet()).unwrap();
        assert!(matches!(batch.items[0].result, ExtractResult::Bundle(_)));
        assert!(out_dir.join("note.txt").is_file());
        assert!(out_dir.join("song.mp3").is_file());
    }

    #[test]
    fn several_bundles_get_their_own_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("note.txt");
        fs::write(&payload, b"shared").unwrap();
        let mut carriers = Vec::new();
        for name in ["first.png", "second.png"] {
            let carrier = dir.path().join(name);
            image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8, y as u8, 3]))
                .save(&carrier)
                .unwrap();
            let extra = dir.path().join("extra.txt");
            fs::write(&extra, b"extra").unwrap();
            let output = dir.path().join(format!("hidden-{name}"));
            super::super::embed_bundle(
                &carrier,
                &[payload.clone(), extra],
                &output,
                &ContainerOptions::default(),
                1,
                &Job::quiet(),
            )
            .unwrap();
            carriers.push(output);
        }

        let out_dir = dir.path().join("restored");
        let batch = extract_many(&carriers, Some(&out_dir), &[], &Job::quiet()).unwrap();
        assert_eq!(batch.items.len(), 2);
        assert!(out_dir.join("hidden-first").join("note.txt").is_file());
        assert!(out_dir.join("hidden-second").join("note.txt").is_file());
    }

    #[test]
    fn probe_finds_the_right_password_for_each_file() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("photo.png");
        image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8, y as u8, 11]))
            .save(&carrier)
            .unwrap();
        let payload = dir.path().join("secret.bin");
        fs::write(&payload, b"encrypted payload").unwrap();
        let output = dir.path().join("hidden.png");
        super::super::embed_file(
            &carrier,
            &payload,
            &output,
            &ContainerOptions {
                compress: false,
                password: Some("second".to_owned()),
                bundle: false,
            },
            1,
            &Job::quiet(),
        )
        .unwrap();

        let passwords = vec!["wrong".to_owned(), "second".to_owned()];
        let (summary, used) = probe(&output, &passwords).unwrap();
        assert!(summary.info.encrypted);
        assert_eq!(used.as_deref(), Some("second"));

        let out_dir = dir.path().join("restored");
        let batch = extract_many(&[output], Some(&out_dir), &passwords, &Job::quiet()).unwrap();
        assert!(batch.items[0].is_done());
        assert_eq!(
            fs::read(out_dir.join("secret.bin")).unwrap(),
            b"encrypted payload"
        );
    }
}
