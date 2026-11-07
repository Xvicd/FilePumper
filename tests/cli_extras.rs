//! Проверка новых возможностей CLI: план без записи, папки и списки файлов,
//! режимы размера, параллельный прогон, отчёты, бандлы и проверка целостности.

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

fn text_file(dir: &Path, name: &str, bytes: usize) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![b'x'; bytes]).unwrap();
    path
}

fn png_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 8, y as u8 * 8, 40]))
        .save(&path)
        .unwrap();
    path
}

#[test]
fn dry_run_shows_plan_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt", 16);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
        "--dry-run",
    ]);

    assert_eq!(code, 0);
    assert!(
        !out_dir.exists(),
        "--dry-run не должен создавать папку результатов"
    );
    assert!(!dir.path().join("note_1MB.txt").exists());
}

#[test]
fn from_dir_recursive_and_extension_filter() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    fs::create_dir_all(&nested).unwrap();
    text_file(dir.path(), "a.txt", 8);
    text_file(&nested, "b.txt", 8);
    text_file(dir.path(), "skip.md", 8);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        "--from-dir",
        path_str(dir.path()),
        "--recursive",
        "--ext",
        "txt",
        "--size",
        "1",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    assert!(out_dir.join("a_1MB.txt").is_file());
    assert!(out_dir.join("b_1MB.txt").is_file());
    assert!(!out_dir.join("skip_1MB.md").exists());
}

#[test]
fn from_file_list_adds_inputs() {
    let dir = tempfile::tempdir().unwrap();
    text_file(dir.path(), "one.txt", 8);
    text_file(dir.path(), "two.txt", 8);
    let list = dir.path().join("inputs.txt");
    fs::write(&list, "# список\none.txt\ntwo.txt\n").unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        "--from-file",
        path_str(&list),
        "--size",
        "1",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    assert!(out_dir.join("one_1MB.txt").is_file());
    assert!(out_dir.join("two_1MB.txt").is_file());
}

#[test]
fn size_modes_add_and_at_least() {
    let dir = tempfile::tempdir().unwrap();
    let small = text_file(dir.path(), "small.txt", 100);
    let out_dir = dir.path().join("out");

    assert_eq!(
        cli(&[
            "pump",
            path_str(&small),
            "--size",
            "1",
            "--size-mode",
            "add",
            "--method",
            "padding",
            "--out-dir",
            path_str(&out_dir),
        ]),
        0
    );
    assert_eq!(
        fs::metadata(out_dir.join("small_1MB.txt")).unwrap().len(),
        MIB + 100
    );

    let big = text_file(dir.path(), "big.txt", 2 * MIB as usize);
    assert_eq!(
        cli(&[
            "pump",
            path_str(&big),
            "--size",
            "1",
            "--size-mode",
            "at-least",
            "--method",
            "padding",
            "--out-dir",
            path_str(&out_dir),
        ]),
        0
    );
    assert_eq!(
        fs::metadata(out_dir.join("big_2MB.txt")).unwrap().len(),
        2 * MIB,
        "файл крупнее цели не должен уменьшаться"
    );
}

#[test]
fn size_list_is_applied_per_file() {
    let dir = tempfile::tempdir().unwrap();
    let first = text_file(dir.path(), "first.txt", 8);
    let second = text_file(dir.path(), "second.txt", 8);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        path_str(&first),
        path_str(&second),
        "--size",
        "1,2",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        fs::metadata(out_dir.join("first_1MB.txt")).unwrap().len(),
        MIB
    );
    assert_eq!(
        fs::metadata(out_dir.join("second_2MB.txt")).unwrap().len(),
        2 * MIB
    );
}

#[test]
fn mismatched_size_list_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let first = text_file(dir.path(), "first.txt", 8);
    let second = text_file(dir.path(), "second.txt", 8);
    let third = text_file(dir.path(), "third.txt", 8);

    let code = cli(&[
        "pump",
        path_str(&first),
        path_str(&second),
        path_str(&third),
        "--size",
        "1,2",
        "--method",
        "padding",
    ]);

    assert_eq!(code, 1, "два размера на три файла — ошибка");
}

#[test]
fn skip_unsupported_exits_zero_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let text = text_file(dir.path(), "note.txt", 8);
    let picture = png_file(dir.path(), "picture.png");
    let out_dir = dir.path().join("out");
    let report = dir.path().join("report.json");

    let code = cli(&[
        "pump",
        path_str(&text),
        path_str(&picture),
        "--size",
        "1",
        "--method",
        "png-chunks",
        "--skip-unsupported",
        "--out-dir",
        path_str(&out_dir),
        "--report",
        path_str(&report),
    ]);

    assert_eq!(code, 0, "неподдерживаемый файл не должен давать код ошибки");
    assert!(out_dir.join("picture_1MB.png").is_file());
    let text_report = fs::read_to_string(&report).unwrap();
    assert!(text_report.contains("\"skipped\": 1"), "{text_report}");
    assert!(text_report.contains("\"status\": \"skipped\""));
}

#[test]
fn jobs_processes_every_file_and_writes_csv_report() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let report = dir.path().join("report.csv");
    let mut argv = vec!["pump".to_owned()];
    for index in 0..5 {
        let path = text_file(dir.path(), &format!("file{index}.txt"), 8);
        argv.push(path.to_string_lossy().into_owned());
    }
    for value in [
        "--size",
        "1",
        "--method",
        "padding",
        "--jobs",
        "3",
        "--out-dir",
        path_str(&out_dir),
        "--report",
        path_str(&report),
    ] {
        argv.push(value.to_owned());
    }

    let code = file_pumper::cli::run(&argv);

    assert_eq!(code, 0);
    for index in 0..5 {
        let path = out_dir.join(format!("file{index}_1MB.txt"));
        assert!(path.is_file(), "нет файла {}", path.display());
        assert_eq!(fs::metadata(path).unwrap().len(), MIB);
    }
    let csv = fs::read_to_string(&report).unwrap();
    assert!(csv.starts_with("input,output,status,bytes,message"));
    assert_eq!(csv.lines().count(), 6);
}

#[test]
fn verify_accepts_flag_after_pump() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "data.txt", 8);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
        "--verify",
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        fs::metadata(out_dir.join("data_1MB.txt")).unwrap().len(),
        MIB
    );
}

#[test]
fn bundle_embed_inspect_verify_and_extract() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "carrier.png");
    let first = text_file(dir.path(), "secret-a.txt", 32);
    let second = text_file(dir.path(), "secret-b.txt", 48);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "embed",
        path_str(&carrier),
        "--payload",
        path_str(&first),
        "--payload",
        path_str(&second),
        "--out-dir",
        path_str(&out_dir),
    ]);
    assert_eq!(code, 0);
    let embedded = out_dir.join("carrier_with_payload.png");
    assert!(embedded.is_file());

    assert_eq!(cli(&["inspect", path_str(&embedded)]), 0);
    assert_eq!(cli(&["verify", path_str(&embedded)]), 0);

    let restored = dir.path().join("restored");
    assert_eq!(
        cli(&[
            "extract",
            path_str(&embedded),
            "--out-dir",
            path_str(&restored),
        ]),
        0
    );
    assert_eq!(
        fs::read(restored.join("secret-a.txt")).unwrap(),
        fs::read(&first).unwrap()
    );
    assert_eq!(
        fs::read(restored.join("secret-b.txt")).unwrap(),
        fs::read(&second).unwrap()
    );
}

#[test]
fn embed_accepts_several_carriers_and_legacy_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let first_carrier = png_file(dir.path(), "one.png");
    let second_carrier = png_file(dir.path(), "two.png");
    let payload = text_file(dir.path(), "secret.txt", 24);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "embed",
        path_str(&first_carrier),
        path_str(&second_carrier),
        "--payload",
        path_str(&payload),
        "--out-dir",
        path_str(&out_dir),
    ]);
    assert_eq!(code, 0);
    for name in ["one_with_payload.png", "two_with_payload.png"] {
        let path = out_dir.join(name);
        assert!(path.is_file(), "нет файла {}", path.display());
        assert_eq!(cli(&["verify", path_str(&path)]), 0);
    }

    let legacy_output = dir.path().join("legacy.png");
    assert_eq!(
        cli(&[
            "embed",
            path_str(&first_carrier),
            path_str(&payload),
            path_str(&legacy_output),
        ]),
        0
    );
    assert!(legacy_output.is_file());
    assert_eq!(cli(&["verify", path_str(&legacy_output)]), 0);
}

#[test]
fn embed_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "carrier.png");
    let payload = text_file(dir.path(), "secret.txt", 16);
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "embed",
        path_str(&carrier),
        "--payload",
        path_str(&payload),
        "--out-dir",
        path_str(&out_dir),
        "--dry-run",
    ]);

    assert_eq!(code, 0);
    assert!(!out_dir.exists());
}

#[test]
fn verify_reports_files_without_payload() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "plain.png");
    assert_eq!(cli(&["verify", path_str(&carrier)]), 1);
}
