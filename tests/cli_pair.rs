//! Групповые команды CLI: пары «носитель ← нагрузка», обзор и извлечение
//! нескольких файлов, поведение при обычных файлах без нагрузки.

use std::fs;
use std::path::{Path, PathBuf};

use file_pumper::payload;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|value| (*value).to_owned()).collect()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn png_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 6, y as u8 * 6, 30]))
        .save(&path)
        .unwrap();
    path
}

fn payload_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    path
}

#[test]
fn pair_mode_puts_each_payload_into_its_own_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let first = png_file(dir.path(), "first.png");
    let second = png_file(dir.path(), "second.png");
    let song = payload_file(dir.path(), "song.mp3", b"id3 pretend audio");
    let note = payload_file(dir.path(), "note.txt", b"plain note");
    let out_dir = dir.path().join("out");

    let code = file_pumper::cli::run(&args(&[
        "embed",
        path_str(&first),
        path_str(&second),
        "--payload",
        path_str(&song),
        "--payload",
        path_str(&note),
        "--pair",
        "--pair-password",
        "первый",
        "--pair-password",
        "второй",
        "--out-dir",
        path_str(&out_dir),
    ]));
    assert_eq!(code, 0);

    let first_out = out_dir.join("first_with_payload.png");
    let second_out = out_dir.join("second_with_payload.png");
    assert!(first_out.is_file(), "нет {}", first_out.display());
    assert!(second_out.is_file(), "нет {}", second_out.display());

    let first_summary = payload::inspect_file(&first_out, Some("первый")).unwrap();
    assert_eq!(first_summary.info.name, "song.mp3");
    assert!(first_summary.info.encrypted);
    let second_summary = payload::inspect_file(&second_out, Some("второй")).unwrap();
    assert_eq!(second_summary.info.name, "note.txt");
    assert!(!second_summary.info.bundle);
}

#[test]
fn pair_mode_with_one_payload_repeats_it_for_every_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let first = png_file(dir.path(), "one.png");
    let second = png_file(dir.path(), "two.png");
    let secret = payload_file(dir.path(), "secret.bin", b"same secret");
    let out_dir = dir.path().join("out");

    let code = file_pumper::cli::run(&args(&[
        "embed",
        path_str(&first),
        path_str(&second),
        "--payload",
        path_str(&secret),
        "--pair",
        "--password",
        "shared",
        "--out-dir",
        path_str(&out_dir),
    ]));
    assert_eq!(code, 0);
    for name in ["one_with_payload.png", "two_with_payload.png"] {
        let summary = payload::inspect_file(&out_dir.join(name), Some("shared")).unwrap();
        assert_eq!(summary.info.name, "secret.bin");
    }
}

#[test]
fn extract_with_a_plain_file_in_the_batch_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "carrier.png");
    let payload_path = payload_file(dir.path(), "song.mp3", b"id3 pretend audio");
    let hidden = dir.path().join("hidden.png");
    payload::embed_file(
        &carrier,
        &payload_path,
        &hidden,
        &payload::ContainerOptions::default(),
        1,
        &file_pumper::util::Job::quiet(),
    )
    .unwrap();
    let plain = png_file(dir.path(), "plain.png");
    let out_dir = dir.path().join("restored");

    let code = file_pumper::cli::run(&args(&[
        "extract",
        path_str(&hidden),
        path_str(&plain),
        "--out-dir",
        path_str(&out_dir),
    ]));
    assert_eq!(code, 0, "обычный файл в пакете не должен ломать команду");
    assert_eq!(
        fs::read(out_dir.join("song.mp3")).unwrap(),
        b"id3 pretend audio"
    );
}

#[test]
fn extract_keeps_working_with_an_explicit_output_name() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "carrier.png");
    let payload_path = payload_file(dir.path(), "song.mp3", b"id3 pretend audio");
    let hidden = dir.path().join("hidden.png");
    payload::embed_file(
        &carrier,
        &payload_path,
        &hidden,
        &payload::ContainerOptions::default(),
        1,
        &file_pumper::util::Job::quiet(),
    )
    .unwrap();

    let target = dir.path().join("renamed");
    let code = file_pumper::cli::run(&args(&[
        "extract",
        path_str(&hidden),
        "--output",
        path_str(&target),
    ]));
    assert_eq!(code, 0);
    assert!(
        dir.path().join("renamed.mp3").is_file(),
        "расширение должно вернуться к имени файла"
    );
}

#[test]
fn extract_of_a_single_plain_file_still_fails() {
    let dir = tempfile::tempdir().unwrap();
    let plain = png_file(dir.path(), "plain.png");
    let code = file_pumper::cli::run(&args(&["extract", path_str(&plain)]));
    assert_eq!(code, 1, "для одного файла без нагрузки ошибка остаётся");
}

#[test]
fn group_extract_tries_every_given_password() {
    let dir = tempfile::tempdir().unwrap();
    let first_carrier = png_file(dir.path(), "first.png");
    let second_carrier = png_file(dir.path(), "second.png");
    let first_payload = payload_file(dir.path(), "song.mp3", b"first secret");
    let second_payload = payload_file(dir.path(), "note.txt", b"second secret");
    let first_hidden = dir.path().join("first-hidden.png");
    let second_hidden = dir.path().join("second-hidden.png");
    for (carrier, payload_path, output, password) in [
        (&first_carrier, &first_payload, &first_hidden, "первый"),
        (&second_carrier, &second_payload, &second_hidden, "второй"),
    ] {
        payload::embed_file(
            carrier,
            payload_path,
            output,
            &payload::ContainerOptions {
                compress: false,
                password: Some(password.to_owned()),
                bundle: false,
            },
            1,
            &file_pumper::util::Job::quiet(),
        )
        .unwrap();
    }

    let out_dir = dir.path().join("restored");
    let code = file_pumper::cli::run(&args(&[
        "extract",
        path_str(&first_hidden),
        path_str(&second_hidden),
        "--password",
        "чужой",
        "--password",
        "первый",
        "--password",
        "второй",
        "--out-dir",
        path_str(&out_dir),
    ]));
    assert_eq!(code, 0, "пароли должны перебираться по порядку");
    assert_eq!(fs::read(out_dir.join("song.mp3")).unwrap(), b"first secret");
    assert_eq!(
        fs::read(out_dir.join("note.txt")).unwrap(),
        b"second secret"
    );
}

#[test]
fn inspect_accepts_a_mixed_list() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "carrier.png");
    let payload_path = payload_file(dir.path(), "note.txt", b"data");
    let hidden = dir.path().join("hidden.png");
    payload::embed_file(
        &carrier,
        &payload_path,
        &hidden,
        &payload::ContainerOptions::default(),
        1,
        &file_pumper::util::Job::quiet(),
    )
    .unwrap();
    let plain = png_file(dir.path(), "plain.png");

    let code = file_pumper::cli::run(&args(&["inspect", path_str(&hidden), path_str(&plain)]));
    assert_eq!(code, 0);
}
