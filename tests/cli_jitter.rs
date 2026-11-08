//! Проверка разброса размера: `--vary` и `--vary-mode`.
//!
//! Разброс не ломает главное обещание программы: файл всё равно создаётся ровно
//! того размера, который выпал, поэтому `--verify` проходит, а размер в имени
//! совпадает с фактическим.

use std::fs;
use std::path::{Path, PathBuf};

use file_pumper::util::MIB;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|value| (*value).to_owned()).collect()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn cli(list: &[&str]) -> i32 {
    file_pumper::cli::run(&args(list))
}

fn text_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"plain text for the pumper\n").unwrap();
    path
}

/// Размеры всех файлов папки (по возрастанию).
fn sizes_in(dir: &Path) -> Vec<u64> {
    let mut sizes: Vec<u64> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .collect();
    sizes.sort_unstable();
    sizes
}

#[test]
fn pump_with_vary_plus_stays_between_the_size_and_the_spread() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let first = text_file(dir.path(), "first.txt");
    let second = text_file(dir.path(), "second.txt");

    let code = cli(&[
        "pump",
        path_str(&first),
        path_str(&second),
        "--size",
        "1",
        "--method",
        "padding",
        "--vary",
        "0.25",
        "--vary-mode",
        "plus",
        "--names",
        "plain",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let sizes = sizes_in(&out_dir);
    assert_eq!(sizes.len(), 2);
    for size in sizes {
        assert!(
            (MIB..=MIB + MIB / 4).contains(&size),
            "размер {size} вне диапазона 1…1,25 МБ"
        );
    }
}

#[test]
fn generate_with_vary_minus_never_exceeds_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "bin",
        "--count",
        "5",
        "--vary",
        "0.5",
        "--vary-mode",
        "minus",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let sizes = sizes_in(&out_dir);
    assert_eq!(sizes.len(), 5);
    for size in &sizes {
        assert!(
            (MIB / 2..=MIB).contains(size),
            "размер {size} вне диапазона 0,5…1 МБ"
        );
    }
    assert!(
        sizes.iter().any(|size| *size != MIB),
        "разброс не сработал: {sizes:?}"
    );
}

#[test]
fn vary_both_keeps_sizes_around_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "2",
        "--type",
        "bin",
        "--count",
        "6",
        "--vary",
        "0.5",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let sizes = sizes_in(&out_dir);
    assert_eq!(sizes.len(), 6);
    for size in &sizes {
        assert!(
            (2 * MIB - MIB / 2..=2 * MIB + MIB / 2).contains(size),
            "размер {size} вне диапазона 1,5…2,5 МБ"
        );
    }
    assert!(
        sizes.iter().any(|size| *size != 2 * MIB),
        "разброс не сработал: {sizes:?}"
    );
}

#[test]
fn verify_confirms_the_chosen_random_size() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "png",
        "--count",
        "2",
        "--vary",
        "0.1",
        "--vary-mode",
        "plus",
        "--out-dir",
        path_str(&out_dir),
        "--verify",
    ]);

    assert_eq!(code, 0, "файл должен совпадать с выпавшим размером");
    let sizes = sizes_in(&out_dir);
    assert_eq!(sizes.len(), 2);
    for size in sizes {
        assert!(
            (MIB..=MIB + MIB / 10).contains(&size),
            "размер {size} вне диапазона"
        );
    }
}

#[test]
fn zero_spread_keeps_the_exact_size() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "0.5",
        "--type",
        "bin",
        "--count",
        "2",
        "--vary",
        "0",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let sizes = sizes_in(&out_dir);
    assert_eq!(sizes, vec![MIB / 2, MIB / 2]);
}

#[test]
fn invalid_spread_and_direction_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    for extra in [
        vec!["--vary", "-1"],
        vec!["--vary", "много"],
        vec!["--vary", "1", "--vary-mode", "sideways"],
    ] {
        let mut list = vec!["generate", "--size", "1", "--out-dir", path_str(&out_dir)];
        list.extend(extra);
        assert_eq!(cli(&list), 1, "флаги {list:?} должны быть отклонены");
    }
    assert!(!out_dir.exists(), "при ошибке ничего не создаётся");
}

#[test]
fn dry_run_prints_the_plan_with_the_spread() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let note = text_file(dir.path(), "note.txt");

    let code = cli(&[
        "pump",
        path_str(&note),
        "--size",
        "1",
        "--method",
        "padding",
        "--vary",
        "0.2",
        "--vary-mode",
        "plus",
        "--dry-run",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    assert!(!out_dir.exists(), "папка результатов не должна создаваться");
}

#[test]
fn size_in_the_name_matches_the_actual_random_size() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "bin",
        "--count",
        "3",
        "--vary",
        "0.5",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    for entry in fs::read_dir(&out_dir).unwrap() {
        let path = entry.unwrap().path();
        let length = fs::metadata(&path).unwrap().len();
        let stem = path.file_stem().and_then(|value| value.to_str()).unwrap();
        let label = stem.rsplit('_').next().unwrap();
        let label = label.split('-').next().unwrap();
        let labelled: f64 = label
            .trim_end_matches("MB")
            .parse()
            .unwrap_or_else(|_| panic!("имя {} без размера", path.display()));
        let actual = length as f64 / MIB as f64;
        assert!(
            (actual - labelled).abs() < 0.01,
            "имя {} врёт про размер {actual:.3} МБ",
            path.display()
        );
    }
}
