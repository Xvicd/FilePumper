//! Проверка пакетных команд CLI: несколько файлов, папка результатов, коды выхода.

use std::fs;
use std::path::{Path, PathBuf};

use file_pumper::payload::{self, ContainerOptions};
use file_pumper::util::Job;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|value| (*value).to_owned()).collect()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn text_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"plain text payload sample\n").unwrap();
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
fn pump_creates_a_file_for_every_input() {
    let dir = tempfile::tempdir().unwrap();
    let first = text_file(dir.path(), "first.txt");
    let second = text_file(dir.path(), "second.txt");
    let out_dir = dir.path().join("out");

    let code = file_pumper::cli::run(&args(&[
        "pump",
        path_str(&first),
        path_str(&second),
        "--size",
        "1",
        "--method",
        "padding",
        "--out-dir",
        path_str(&out_dir),
    ]));

    assert_eq!(code, 0);
    for name in ["first_1MB.txt", "second_1MB.txt"] {
        let path = out_dir.join(name);
        assert!(path.is_file(), "нет файла {}", path.display());
        assert_eq!(fs::metadata(&path).unwrap().len(), 1024 * 1024);
    }
}

#[test]
fn pump_keeps_going_when_one_file_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let text = text_file(dir.path(), "note.txt");
    let picture = png_file(dir.path(), "picture.png");
    let out_dir = dir.path().join("out");

    let code = file_pumper::cli::run(&args(&[
        "pump",
        path_str(&text),
        path_str(&picture),
        "--size",
        "1",
        "--method",
        "png-chunks",
        "--out-dir",
        path_str(&out_dir),
    ]));

    assert_eq!(code, 1, "пакет с ошибкой должен вернуть код 1");
    assert!(out_dir.join("picture_1MB.png").is_file());
    assert!(!out_dir.join("note_1MB.png").exists());
}

#[test]
fn legacy_two_path_syntax_writes_exact_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "input.txt");
    let output = dir.path().join("result.bin");

    let code = file_pumper::cli::run(&args(&[
        "pump",
        path_str(&input),
        path_str(&output),
        "--size",
        "1",
        "--method",
        "padding",
    ]));

    assert_eq!(code, 0);
    assert_eq!(fs::metadata(&output).unwrap().len(), 1024 * 1024);
}

#[test]
fn output_option_writes_exact_path() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "input.txt");
    let output = dir.path().join("exact_name.dat");

    let code = file_pumper::cli::run(&args(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--output",
        path_str(&output),
    ]));

    assert_eq!(code, 0);
    assert_eq!(fs::metadata(&output).unwrap().len(), 1024 * 1024);
}

#[test]
fn pump_does_not_overwrite_previous_result() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "data.txt");

    for _ in 0..2 {
        let code = file_pumper::cli::run(&args(&[
            "pump",
            path_str(&input),
            "--size",
            "1",
            "--method",
            "padding",
        ]));
        assert_eq!(code, 0);
    }

    assert!(dir.path().join("data_1MB.txt").is_file());
    assert!(dir.path().join("data_1MB-2.txt").is_file());
}

fn carrier_with_payload(dir: &Path, carrier_name: &str, payload_bytes: &[u8]) -> PathBuf {
    let carrier = png_file(dir, carrier_name);
    let payload_path = dir.join(format!("{carrier_name}.payload"));
    fs::write(&payload_path, payload_bytes).unwrap();
    let output = dir.join(format!("{carrier_name}.hidden.png"));
    payload::embed_file(
        &carrier,
        &payload_path,
        &output,
        &ContainerOptions::default(),
        1,
        &Job::quiet(),
    )
    .unwrap();
    output
}

#[test]
fn extract_and_inspect_accept_several_carriers() {
    let dir = tempfile::tempdir().unwrap();
    let first_bytes = b"first secret".to_vec();
    let second_bytes = b"second secret".to_vec();
    let first = carrier_with_payload(dir.path(), "one.png", &first_bytes);
    let second = carrier_with_payload(dir.path(), "two.png", &second_bytes);
    let out_dir = dir.path().join("restored");

    let inspect_code =
        file_pumper::cli::run(&args(&["inspect", path_str(&first), path_str(&second)]));
    assert_eq!(inspect_code, 0);

    let extract_code = file_pumper::cli::run(&args(&[
        "extract",
        path_str(&first),
        path_str(&second),
        "--out-dir",
        path_str(&out_dir),
    ]));
    assert_eq!(extract_code, 0);

    let mut names: Vec<String> = fs::read_dir(&out_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "ожидались два извлечённых файла: {names:?}");

    let mut contents: Vec<Vec<u8>> = names
        .iter()
        .map(|name| fs::read(out_dir.join(name)).unwrap())
        .collect();
    contents.sort();
    let mut expected = vec![first_bytes, second_bytes];
    expected.sort();
    assert_eq!(contents, expected);
}
