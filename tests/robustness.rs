//! Устойчивость: обрезка хвоста сервером, повреждение данных, старые контейнеры.

use std::fs;
use std::path::{Path, PathBuf};

use file_pumper::payload::format::{
    build_container, parse_container, pick_container, ContainerOptions,
};
use file_pumper::payload::{self, Carrier};
use file_pumper::util::{crc32_bytes, Job};

fn sample(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| (index as u32 * 37 % 251) as u8)
        .collect()
}

fn make_png(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(6, 6, |x, y| image::Rgb([x as u8, y as u8, 7]))
        .save(&path)
        .unwrap();
    path
}

#[test]
fn build_and_parse_roundtrip_with_all_flags() {
    let bytes = sample(4096);
    let plain = build_container(
        &bytes,
        "file.bin",
        &ContainerOptions::default(),
        &Job::quiet(),
    )
    .unwrap();
    let parsed = parse_container(&plain, None).unwrap();
    assert_eq!(parsed.payload, bytes);
    assert!(!parsed.compressed && !parsed.encrypted);

    let options = ContainerOptions {
        compress: true,
        password: Some("pw".to_owned()),
        bundle: false,
    };
    let secure = build_container(&bytes, "file.bin", &options, &Job::quiet()).unwrap();
    assert!(secure.len() < plain.len());
    assert!(parse_container(&secure, None).is_err());
    let parsed = parse_container(&secure, Some("pw")).unwrap();
    assert_eq!(parsed.payload, bytes);
    assert!(parsed.compressed && parsed.encrypted);
}

#[test]
fn truncated_container_is_reported_instead_of_panicking() {
    let container = build_container(
        &sample(5000),
        "a.bin",
        &ContainerOptions::default(),
        &Job::quiet(),
    )
    .unwrap();
    for cut in [1_usize, 9, 40, 512, container.len() - 1] {
        let truncated = &container[..container.len() - cut];
        assert!(parse_container(truncated, None).is_err(), "cut={cut}");
    }
}

#[test]
fn corrupted_payload_and_header_are_detected() {
    let mut container = build_container(
        &sample(2048),
        "a.bin",
        &ContainerOptions::default(),
        &Job::quiet(),
    )
    .unwrap();
    let payload_offset = container.len() / 2;
    container[payload_offset] ^= 0xFF;
    assert!(parse_container(&container, None).is_err());

    let mut container = build_container(
        &sample(2048),
        "a.bin",
        &ContainerOptions::default(),
        &Job::quiet(),
    )
    .unwrap();
    container[9] ^= 0xFF; // имя файла внутри заголовка
    assert!(parse_container(&container, None).is_err());
}

#[test]
fn legacy_version_one_container_is_still_readable() {
    let payload = b"legacy payload";
    let mut header = Vec::new();
    header.extend_from_slice(b"UFPL");
    header.push(1);
    header.push(0);
    header.extend_from_slice(&(5_u16).to_be_bytes());
    header.extend_from_slice(b"o.bin");
    header.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    header.extend_from_slice(&crc32_bytes(payload).to_be_bytes());
    let header_crc = crc32_bytes(&header);
    header.extend_from_slice(&header_crc.to_be_bytes());

    let total = header.len() as u64 + payload.len() as u64 + 12;
    let mut container = header;
    container.extend_from_slice(payload);
    container.extend_from_slice(b"LFPU");
    container.extend_from_slice(&total.to_be_bytes());

    let parsed = parse_container(&container, None).unwrap();
    assert!(parsed.legacy);
    assert_eq!(parsed.name, "o.bin");
    assert_eq!(parsed.payload, payload);
}

#[test]
fn payload_survives_when_server_cuts_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = dir.path().join("carrier.bin");
    fs::write(&carrier, b"plain carrier\n".repeat(50)).unwrap();
    let carrier_len = fs::metadata(&carrier).unwrap().len();
    let payload_path = dir.path().join("secret.bin");
    let bytes = sample(20_000);
    fs::write(&payload_path, &bytes).unwrap();

    let output = dir.path().join("raw-with-payload.bin");
    payload::embed_file(
        &carrier,
        &payload_path,
        &output,
        &ContainerOptions::default(),
        3,
        &Job::quiet(),
    )
    .unwrap();

    let total = fs::metadata(&output).unwrap().len();
    let one_copy = (total - carrier_len) / 3;
    // Сервер обрезает последнюю копию вместе с концевым маркером,
    // первые две остаются целыми.
    let file = fs::OpenOptions::new().write(true).open(&output).unwrap();
    file.set_len(carrier_len + one_copy * 2 + 64).unwrap();
    drop(file);

    let (parsed, copies, kind) = payload::extract_payload(&output, None).unwrap();
    assert_eq!(kind, Carrier::Raw);
    assert!(copies >= 2, "целых копий: {copies}");
    assert_eq!(parsed.payload, bytes);
}

#[test]
fn png_payload_survives_partial_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = make_png(dir.path(), "picture.png");
    let payload_path = dir.path().join("secret.bin");
    let bytes = sample(30_000);
    fs::write(&payload_path, &bytes).unwrap();

    let output = dir.path().join("png-with-payload.png");
    payload::embed_file(
        &carrier,
        &payload_path,
        &output,
        &ContainerOptions::default(),
        3,
        &Job::quiet(),
    )
    .unwrap();

    let total = fs::metadata(&output).unwrap().len();
    let file = fs::OpenOptions::new().write(true).open(&output).unwrap();
    file.set_len(total / 2).unwrap();
    drop(file);

    let (parsed, copies, kind) = payload::extract_payload(&output, None).unwrap();
    assert_eq!(kind, Carrier::Png);
    assert!(copies >= 1);
    assert_eq!(parsed.payload, bytes);
}

#[test]
fn plain_files_report_missing_payload() {
    let dir = tempfile::tempdir().unwrap();
    let png = make_png(dir.path(), "clean.png");
    let raw = dir.path().join("clean.bin");
    fs::write(&raw, b"nothing hidden here".repeat(10)).unwrap();
    for path in [png, raw] {
        let error = payload::inspect_file(&path, None).unwrap_err();
        assert!(error.contains("не найдена"), "{error}");
    }
}

#[test]
fn pick_container_finds_copy_behind_garbage() {
    let bytes = sample(1024);
    let container =
        build_container(&bytes, "x.bin", &ContainerOptions::default(), &Job::quiet()).unwrap();
    let mut stack = b"noise before".to_vec();
    stack.extend_from_slice(&container);
    stack.extend_from_slice(b"noise after");
    let (parsed, copies) = pick_container(&stack, None).unwrap();
    assert_eq!(copies, 1);
    assert_eq!(parsed.payload, bytes);
}
