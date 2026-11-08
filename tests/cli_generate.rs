//! Проверка команды `generate`: файлы создаются с нуля, не сжимаются,
//! а нагрузка (второе дно) встаёт внутрь, не увеличивая размер.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use file_pumper::batch::{self, NameMode};
use file_pumper::pumper::GeneratedKind;
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

fn text_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Сколько байт займёт файл после deflate-сжатия: у несжимаемых данных
/// упакованный размер почти не меньше исходного.
fn deflate_len(bytes: &[u8]) -> u64 {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap().len() as u64
}

fn made_name(base: &str, target: u64, kind: GeneratedKind, names: NameMode) -> String {
    batch::generated_name(base, target, kind, names)
}

#[test]
fn plain_file_is_created_with_exact_size_and_cannot_be_compressed() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let target = MIB;

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "bin",
        "--name",
        "noise",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let made = out_dir.join(made_name(
        "noise",
        target,
        GeneratedKind::Bin,
        NameMode::Size,
    ));
    let bytes = fs::read(&made).unwrap();
    assert_eq!(bytes.len() as u64, target);
    let packed = deflate_len(&bytes);
    assert!(
        packed as f64 > target as f64 * 0.95,
        "файл сжался до {packed} из {target}"
    );
}

#[test]
fn png_kind_is_a_valid_image_of_the_requested_size() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let target = MIB;

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "png",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let made = out_dir.join(made_name(
        "file",
        target,
        GeneratedKind::Png,
        NameMode::Size,
    ));
    assert_eq!(fs::metadata(&made).unwrap().len(), target);
    let picture = image::open(&made).expect("PNG должен открываться");
    assert_eq!(picture.width(), 16);
    assert_eq!(picture.height(), 16);
}

#[test]
fn count_creates_several_files_with_distinct_names() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let target = (0.2 * MIB as f64) as u64;

    let code = cli(&[
        "generate",
        "--size",
        "0.2",
        "--count",
        "3",
        "--type",
        "gif",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let mut names: Vec<PathBuf> = fs::read_dir(&out_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    names.sort();
    assert_eq!(names.len(), 3);
    for name in &names {
        assert_eq!(fs::metadata(name).unwrap().len(), target);
        assert!(image::open(name).is_ok(), "{} не открылся", name.display());
    }
}

#[test]
fn every_size_is_repeated_for_every_count() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let small = (0.25 * MIB as f64) as u64;
    let large = (0.5 * MIB as f64) as u64;

    let code = cli(&[
        "generate",
        "--size",
        "0.25,0.5",
        "--count",
        "2",
        "--type",
        "bin",
        "--name",
        "seed",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    // Каждый размер из --size повторяется --count раз, имена не сталкиваются.
    for name in [
        "seed_0.25MB.bin",
        "seed_0.25MB-2.bin",
        "seed_0.5MB.bin",
        "seed_0.5MB-2.bin",
    ] {
        let path = out_dir.join(name);
        assert!(path.is_file(), "нет файла {}", path.display());
    }
    let mut sizes: Vec<u64> = fs::read_dir(&out_dir)
        .unwrap()
        .map(|entry| fs::metadata(entry.unwrap().path()).unwrap().len())
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![small, small, large, large]);
}

#[test]
fn name_modes_follow_the_batch_conventions() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let target = MIB;

    for extra in [
        ["--names", "size"],
        ["--names", "plain"],
        ["--names", "random"],
    ] {
        let mut arguments = vec![
            "generate",
            "--size",
            "1",
            "--type",
            "png",
            "--name",
            "photo",
            "--out-dir",
            path_str(&out_dir),
        ];
        arguments.extend(extra);
        assert_eq!(cli(&arguments), 0, "режим {}", extra[1]);
    }

    assert!(out_dir
        .join(made_name(
            "photo",
            target,
            GeneratedKind::Png,
            NameMode::Size
        ))
        .is_file());
    assert!(out_dir
        .join(made_name(
            "photo",
            target,
            GeneratedKind::Png,
            NameMode::Plain
        ))
        .is_file());
    let random: Vec<PathBuf> = fs::read_dir(&out_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|stem| !stem.starts_with("photo"))
        })
        .collect();
    assert_eq!(random.len(), 1, "должно быть одно случайное имя");
}

#[test]
fn payload_is_hidden_inside_without_growing_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let secret = text_file(dir.path(), "secret.txt", b"payload inside generated png\n");
    let target = MIB;

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "png",
        "--name",
        "photo",
        "--payload",
        path_str(&secret),
        "--password",
        "1234",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let made = out_dir.join(made_name(
        "photo",
        target,
        GeneratedKind::Png,
        NameMode::Size,
    ));
    assert_eq!(
        fs::metadata(&made).unwrap().len(),
        target,
        "второе дно не должно увеличивать файл"
    );
    assert!(
        image::open(&made).is_ok(),
        "картинка должна остаться рабочей"
    );

    assert_eq!(
        cli(&["verify", path_str(&made), "--password", "1234"]),
        0,
        "verify должен подтвердить целостность контейнера"
    );

    let extract_dir = dir.path().join("extract");
    assert_eq!(
        cli(&[
            "extract",
            path_str(&made),
            "--password",
            "1234",
            "--out-dir",
            path_str(&extract_dir),
        ]),
        0
    );
    assert_eq!(
        fs::read(extract_dir.join("secret.txt")).unwrap(),
        b"payload inside generated png\n"
    );
}

#[test]
fn several_payloads_are_stored_as_a_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let first = text_file(dir.path(), "one.txt", b"first\n");
    let second = text_file(dir.path(), "two.txt", b"second\n");
    let target = MIB;

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "gif",
        "--payload",
        path_str(&first),
        "--payload",
        path_str(&second),
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let made = out_dir.join(made_name(
        "file",
        target,
        GeneratedKind::Gif,
        NameMode::Size,
    ));
    assert_eq!(fs::metadata(&made).unwrap().len(), target);
    assert_eq!(cli(&["verify", path_str(&made)]), 0);

    let extract_dir = dir.path().join("extract");
    assert_eq!(
        cli(&[
            "extract",
            path_str(&made),
            "--out-dir",
            path_str(&extract_dir),
        ]),
        0
    );
    assert_eq!(fs::read(extract_dir.join("one.txt")).unwrap(), b"first\n");
    assert_eq!(fs::read(extract_dir.join("two.txt")).unwrap(), b"second\n");
}

#[test]
fn dry_run_prints_the_plan_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "2",
        "--count",
        "2",
        "--type",
        "webp",
        "--out-dir",
        path_str(&out_dir),
        "--dry-run",
    ]);

    assert_eq!(code, 0);
    assert!(!out_dir.exists(), "папка результатов не должна создаваться");
}

#[test]
fn unknown_type_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "exe",
        "--out-dir",
        path_str(&dir.path().join("out")),
    ]);
    assert_eq!(code, 1);
}

#[test]
fn target_smaller_than_the_image_skeleton_fails() {
    let dir = tempfile::tempdir().unwrap();
    let code = cli(&[
        "generate",
        "--size",
        "0.0001",
        "--type",
        "png",
        "--out-dir",
        path_str(&dir.path().join("out")),
    ]);
    assert_eq!(code, 1, "104 байт не хватит даже на служебный PNG");
}

#[test]
fn webp_with_payload_stays_a_working_image() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    let secret = text_file(dir.path(), "secret.txt", b"payload inside generated webp\n");
    let target = MIB;

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "webp",
        "--payload",
        path_str(&secret),
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let made = out_dir.join(made_name(
        "file",
        target,
        GeneratedKind::Webp,
        NameMode::Size,
    ));
    assert_eq!(fs::metadata(&made).unwrap().len(), target);
    assert!(
        image::open(&made).is_ok(),
        "мусор генератора не должен попадать внутрь RIFF"
    );
    assert_eq!(
        cli(&["verify", path_str(&made)]),
        0,
        "контейнер должен читаться и после дополнения"
    );

    let extract_dir = dir.path().join("extract");
    assert_eq!(
        cli(&[
            "extract",
            path_str(&made),
            "--out-dir",
            path_str(&extract_dir),
        ]),
        0
    );
    assert_eq!(
        fs::read(extract_dir.join("secret.txt")).unwrap(),
        b"payload inside generated webp\n"
    );
}
