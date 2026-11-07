//! Сбор входных файлов: папки, рекурсия, фильтры по расширению, списки из файла
//! и предупреждения о размерах, которые не переживут обычные файловые системы.

use std::fs;
use std::path::{Path, PathBuf};

use crate::pumper::Method;
use crate::util::MIB;

/// `--ext png,jpg` → `["png", "jpg"]` (регистр и точки не важны).
pub fn parse_extensions(value: &str) -> Vec<String> {
    value
        .split([',', ';', ' '])
        .map(|item| item.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .is_some_and(|extension| extensions.contains(&extension))
}

/// Все файлы папки; при `recursive` — и вложенных папок. Скрытые файлы и папки
/// (`.git`, `.thumbnails`) пропускаются, порядок стабильный: сортировка по пути.
pub fn collect_from_dir(
    dir: &Path,
    recursive: bool,
    extensions: &[String],
) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("Папка не найдена: {}", dir.display()));
    }
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .map_err(|err| format!("Не удалось прочитать {}: {err}", current.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            if is_hidden(&path) {
                continue;
            }
            let file_type = entry.file_type().map_err(|err| err.to_string())?;
            if file_type.is_dir() {
                if recursive {
                    stack.push(path);
                }
            } else if file_type.is_file() && matches_extension(&path, extensions) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Список путей из текстового файла: по одному в строке, `#` — комментарий,
/// пустые строки пропускаются, относительные пути считаются от папки списка.
pub fn read_path_list(list: &Path) -> Result<Vec<PathBuf>, String> {
    let text = fs::read_to_string(list)
        .map_err(|err| format!("Не удалось прочитать список {}: {err}", list.display()))?;
    let base = list
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut paths = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let candidate = PathBuf::from(trimmed);
        paths.push(if candidate.is_absolute() {
            candidate
        } else {
            base.join(candidate)
        });
    }
    Ok(paths)
}

/// Раскрывает пути, добавленные перетаскиванием: папки обходятся рекурсивно,
/// файлы остаются как есть.
pub fn expand_dropped(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    for path in paths {
        if path.is_dir() {
            match collect_from_dir(path, true, &[]) {
                Ok(files) => expanded.extend(files),
                Err(_) => expanded.push(path.clone()),
            }
        } else {
            expanded.push(path.clone());
        }
    }
    expanded
}

/// Убирает повторы путей, сохраняя порядок (сравнение по каноническому пути).
pub fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            result.push(path);
        }
    }
    result
}

/// Предупреждения о размерах, при которых файл может не открыться или не
/// поместиться на носитель. Пустой список — всё в порядке.
pub fn limits_warnings(target_bytes: u64, method: Method) -> Vec<String> {
    let mut warnings = Vec::new();
    let mib = MIB as f64;
    if target_bytes > (2 * 1024) * MIB {
        warnings.push(format!(
            "Размер {:.1} ГБ: старые редакторы и 32-битные программы могут не открыть файл.",
            target_bytes as f64 / mib / 1024.0
        ));
    }
    if target_bytes > (4 * 1024) * MIB && !matches!(method, Method::Upscale) {
        warnings.push(
            "Размер больше 4 ГБ: FAT32 (обычные флешки) такой файл не примет — нужен NTFS или exFAT."
                .to_owned(),
        );
    }
    if target_bytes > (4 * 1024) * MIB && matches!(method, Method::Upscale) {
        warnings.push(
            "Upscale для такого объёма потребует десятки гигабайт памяти — выберите другой метод."
                .to_owned(),
        );
    }
    if matches!(method, Method::PngChunks | Method::Upscale) && target_bytes > (2 * 1024) * MIB {
        warnings.push(
            "PNG такого размера плохо открывается просмотрщиками — для больших объёмов надёжнее padding."
                .to_owned(),
        );
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.PNG"), b"x").unwrap();
        fs::write(dir.path().join("b.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested").join("c.png"), b"x").unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join(".hidden").join("d.png"), b"x").unwrap();
        dir
    }

    #[test]
    fn extensions_are_normalised() {
        assert_eq!(
            parse_extensions(" .PNG, jpg;;webp "),
            ["png", "jpg", "webp"]
        );
        assert!(parse_extensions("  ").is_empty());
    }

    #[test]
    fn collection_respects_recursion_and_filters() {
        let dir = make_tree();
        let flat = collect_from_dir(dir.path(), false, &[]).unwrap();
        assert_eq!(flat.len(), 2);

        let recursive = collect_from_dir(dir.path(), true, &parse_extensions("png")).unwrap();
        assert_eq!(recursive.len(), 2);
        assert!(recursive
            .iter()
            .all(|path| path.ends_with("a.PNG") || path.ends_with("c.png")));

        assert!(collect_from_dir(&dir.path().join("nope"), false, &[]).is_err());
    }

    #[test]
    fn path_list_skips_comments_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("inputs.txt");
        fs::write(&list, "# комментарий\nfirst.txt\n\nsub/second.txt\n").unwrap();
        let paths = read_path_list(&list).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], dir.path().join("first.txt"));
        assert_eq!(paths[1], dir.path().join("sub").join("second.txt"));
    }

    #[test]
    fn dropped_folders_are_expanded_recursively() {
        let dir = make_tree();
        let expanded = expand_dropped(&[dir.path().join("nested")]);
        assert_eq!(expanded.len(), 1);
        assert!(expanded[0].ends_with("c.png"));
    }

    #[test]
    fn dedupe_removes_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.bin");
        fs::write(&file, b"x").unwrap();
        let unique = dedupe(vec![file.clone(), file.clone(), dir.path().join("b.bin")]);
        assert_eq!(unique.len(), 2);
    }

    #[test]
    fn limits_warn_about_big_targets() {
        assert!(limits_warnings(10 * MIB, Method::Padding).is_empty());
        let warnings = limits_warnings(5 * 1024 * MIB, Method::Padding);
        assert_eq!(warnings.len(), 2);
        assert!(limits_warnings(3 * 1024 * MIB, Method::Upscale)
            .iter()
            .any(|text| text.contains("PNG")));
    }
}
