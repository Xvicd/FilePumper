//! Проверка режимов именования результатов: размер в имени, без него и случайное.

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

/// `quiet-river-4821`: два латинских слова и четырёхзначное число.
fn is_tidy_random_stem(stem: &str) -> bool {
    let parts: Vec<&str> = stem.split('-').collect();
    parts.len() == 3
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && parts[0].chars().all(|value| value.is_ascii_lowercase())
        && parts[1].chars().all(|value| value.is_ascii_lowercase())
        && parts[2].len() == 4
        && parts[2].chars().all(|value| value.is_ascii_digit())
}

/// Файлы папки без учёта исходников (их имена известны заранее).
fn made_files(dir: &Path, originals: &[&Path]) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .filter(|path| !originals.contains(&path.as_path()))
        .collect();
    names.sort();
    names
}

#[test]
fn names_plain_drops_the_size_for_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let first = text_file(dir.path(), "first.txt");
    let second = text_file(dir.path(), "second.txt");
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        path_str(&first),
        path_str(&second),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "plain",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    for name in ["first.txt", "second.txt"] {
        let path = out_dir.join(name);
        assert!(path.is_file(), "нет файла {}", path.display());
        assert_eq!(fs::metadata(&path).unwrap().len(), MIB);
    }
    // Пометки размера не осталось ни в одном имени.
    for entry in fs::read_dir(&out_dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        assert!(!name.contains("MB"), "в имени остался размер: {name}");
    }
}

#[test]
fn names_size_is_the_default_and_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt");
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "size",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    assert!(out_dir.join("note_1MB.txt").is_file());
}

#[test]
fn names_plain_next_to_the_source_never_overwrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "plain",
    ]);

    assert_eq!(code, 0);
    assert_eq!(
        fs::read(&input).unwrap(),
        b"plain text payload sample\n".to_vec(),
        "исходник не должен меняться"
    );
    let created = made_files(dir.path(), &[input.as_path()]);
    assert_eq!(created.len(), 1, "создано: {created:?}");
    let created_name = created[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(created_name, "note-2.txt");
    assert_eq!(fs::metadata(&created[0]).unwrap().len(), MIB);
}

#[test]
fn names_random_gives_every_file_a_tidy_unique_name() {
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<PathBuf> = (0..3)
        .map(|index| text_file(dir.path(), &format!("file{index}.txt")))
        .collect();
    let out_dir = dir.path().join("out");

    let mut list = vec![String::from("pump")];
    list.extend(inputs.iter().map(|path| path.to_string_lossy().to_string()));
    list.extend(args(&[
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "random",
        "--out-dir",
        &out_dir.to_string_lossy(),
    ]));
    let code = file_pumper::cli::run(&list);
    assert_eq!(code, 0);

    let created = made_files(&out_dir, &[]);
    assert_eq!(created.len(), 3, "создано: {created:?}");
    let mut stems: Vec<String> = Vec::new();
    for path in &created {
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("txt")
        );
        assert_eq!(fs::metadata(path).unwrap().len(), MIB);
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_owned();
        assert!(is_tidy_random_stem(&stem), "неприличное имя: {stem}");
        stems.push(stem);
    }
    stems.sort();
    stems.dedup();
    assert_eq!(stems.len(), 3, "имена повторились: {stems:?}");
}

#[test]
fn names_random_also_works_for_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = png_file(dir.path(), "picture.png");
    let payload = text_file(dir.path(), "secret.txt");
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "embed",
        path_str(&carrier),
        "--payload",
        path_str(&payload),
        "--names",
        "random",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let created = made_files(&out_dir, &[]);
    assert_eq!(created.len(), 1, "создано: {created:?}");
    let path = &created[0];
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("png")
    );
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap()
        .to_owned();
    assert!(is_tidy_random_stem(&stem), "неприличное имя: {stem}");

    // Носитель со случайным именем читается: нагрузка на месте.
    let code = cli(&["verify", path_str(path)]);
    assert_eq!(code, 0);
}

#[test]
fn names_random_chars_style_uses_length_and_alphabet() {
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<PathBuf> = (0..2)
        .map(|index| text_file(dir.path(), &format!("src{index}.txt")))
        .collect();
    let out_dir = dir.path().join("out");

    let mut list = vec![String::from("pump")];
    list.extend(inputs.iter().map(|path| path.to_string_lossy().to_string()));
    list.extend(args(&[
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "random",
        "--random-style",
        "chars",
        "--random-length",
        "6",
        "--random-alphabet",
        "ab",
        "--out-dir",
        &out_dir.to_string_lossy(),
    ]));
    let code = file_pumper::cli::run(&list);
    assert_eq!(code, 0);

    let created = made_files(&out_dir, &[]);
    assert_eq!(created.len(), 2, "создано: {created:?}");
    for path in &created {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_owned();
        assert_eq!(stem.chars().count(), 6, "длина имени: {stem}");
        assert!(
            stem.chars().all(|symbol| symbol == 'a' || symbol == 'b'),
            "в имени чужие символы: {stem}"
        );
    }
}

#[test]
fn names_random_chars_style_works_for_generate() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");

    let code = cli(&[
        "generate",
        "--size",
        "1",
        "--type",
        "bin",
        "--names",
        "random",
        "--random-style",
        "chars",
        "--random-length",
        "12",
        "--random-alphabet",
        "0123456789",
        "--out-dir",
        path_str(&out_dir),
    ]);

    assert_eq!(code, 0);
    let created = made_files(&out_dir, &[]);
    assert_eq!(created.len(), 1, "создано: {created:?}");
    let stem = created[0]
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap()
        .to_owned();
    assert_eq!(stem.len(), 12, "длина имени: {stem}");
    assert!(
        stem.chars().all(|symbol| symbol.is_ascii_digit()),
        "в имени не только цифры: {stem}"
    );
}

#[test]
fn random_alphabet_without_usable_symbols_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "random",
        "--random-style",
        "chars",
        "--random-alphabet",
        "абв",
    ]);

    assert_eq!(code, 1);
}

#[test]
fn random_length_out_of_range_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "random",
        "--random-length",
        "1",
    ]);

    assert_eq!(code, 1);
}

#[test]
fn unknown_name_mode_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = text_file(dir.path(), "note.txt");

    let code = cli(&[
        "pump",
        path_str(&input),
        "--size",
        "1",
        "--method",
        "padding",
        "--names",
        "мусор",
    ]);

    assert_eq!(code, 1);
}
