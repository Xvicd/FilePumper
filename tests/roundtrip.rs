//! Сквозные тесты: встраивание и извлечение на всех типах носителей.

use std::fs;
use std::path::{Path, PathBuf};

use file_pumper::payload::{self, Carrier, ContainerOptions};
use file_pumper::util::Job;

fn make_png(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 8, y as u8 * 8, 40]))
        .save(&path)
        .unwrap();
    path
}

fn make_jpeg(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(16, 16, |x, y| image::Rgb([x as u8, y as u8, 90]))
        .save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    path
}

fn make_gif(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(5, 5, |x, y| image::Rgb([x as u8, 10, y as u8]))
        .save_with_format(&path, image::ImageFormat::Gif)
        .unwrap();
    path
}

/// image 0.25 умеет только декодировать WebP, поэтому носитель собирается вручную:
/// RIFF-заголовок и один чанк. Нам важен разбор структуры RIFF, а не картинка.
fn make_webp(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let mut data = Vec::new();
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(4_u32 + 8 + 16).to_le_bytes());
    data.extend_from_slice(b"WEBP");
    data.extend_from_slice(b"VP8 ");
    data.extend_from_slice(&16_u32.to_le_bytes());
    data.extend_from_slice(&[0x2f, 0x01, 0x40, 0x00, 0x10, 0x10, 0x00, 0x00]);
    data.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    fs::write(&path, data).unwrap();
    path
}

fn make_svg(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">\n<rect width=\"10\" height=\"10\" fill=\"#123456\"/>\n</svg>\n",
    )
    .unwrap();
    path
}

fn make_raw(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"plain file without structure\n".repeat(20)).unwrap();
    path
}

fn all_carriers(dir: &Path) -> Vec<(Carrier, PathBuf)> {
    vec![
        (Carrier::Png, make_png(dir, "carrier.png")),
        (Carrier::Jpeg, make_jpeg(dir, "carrier.jpg")),
        (Carrier::Gif, make_gif(dir, "carrier.gif")),
        (Carrier::Webp, make_webp(dir, "carrier.webp")),
        (Carrier::Svg, make_svg(dir, "carrier.svg")),
        (Carrier::Raw, make_raw(dir, "carrier.bin")),
    ]
}

fn sample_payload() -> Vec<u8> {
    (0..40_000_u32)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

fn with_extension(base: &Path, extension: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    path.set_extension(extension);
    path
}

#[test]
fn every_carrier_round_trips_without_options() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = sample_payload();
    let payload_path = dir.path().join("hidden.bin");
    fs::write(&payload_path, &bytes).unwrap();
    let options = ContainerOptions::default();

    for (carrier, carrier_path) in all_carriers(dir.path()) {
        let extension = carrier_path
            .extension()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let output = with_extension(&dir.path().join(format!("out-{carrier:?}")), &extension);
        let outcome = payload::embed_file(
            &carrier_path,
            &payload_path,
            &output,
            &options,
            1,
            &Job::quiet(),
        )
        .unwrap_or_else(|err| panic!("{carrier:?}: {err}"));
        assert_eq!(outcome.carrier, carrier);
        assert_eq!(outcome.copies, 1);

        let summary = payload::inspect_file(&output, None).unwrap();
        assert_eq!(summary.info.name, "hidden.bin");
        assert_eq!(summary.info.size, bytes.len() as u64);
        assert_eq!(summary.info.copies, 1);

        let restored = dir.path().join(format!("restored-{carrier:?}.bin"));
        let result = payload::extract_to(&output, &restored, None, &Job::quiet()).unwrap();
        assert_eq!(result.size, bytes.len() as u64);
        assert_eq!(fs::read(&restored).unwrap(), bytes, "{carrier:?}");
    }
}

#[test]
fn compression_and_password_work_on_every_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = vec![b'X'; 30_000];
    let payload_path = dir.path().join("repeat.bin");
    fs::write(&payload_path, &bytes).unwrap();
    let options = ContainerOptions {
        compress: true,
        password: Some("top-secret".to_owned()),
    };

    for (carrier, carrier_path) in all_carriers(dir.path()) {
        let extension = carrier_path
            .extension()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let output = with_extension(&dir.path().join(format!("secure-{carrier:?}")), &extension);
        payload::embed_file(
            &carrier_path,
            &payload_path,
            &output,
            &options,
            2,
            &Job::quiet(),
        )
        .unwrap_or_else(|err| panic!("{carrier:?}: {err}"));

        let summary = payload::inspect_file(&output, None).unwrap();
        assert!(summary.info.compressed);
        assert!(summary.info.encrypted);
        assert_eq!(summary.info.copies, 2);
        assert_eq!(summary.info.size, bytes.len() as u64);

        let restored = dir.path().join(format!("unpacked-{carrier:?}.bin"));
        assert!(
            payload::extract_to(&output, &restored, Some("wrong"), &Job::quiet()).is_err(),
            "{carrier:?}"
        );
        let result = payload::extract_to(&output, &restored, Some("top-secret"), &Job::quiet())
            .unwrap_or_else(|error| panic!("{carrier:?} {restored:?}: {error}"));
        assert_eq!(result.size, bytes.len() as u64);
        assert_eq!(fs::read(&restored).unwrap(), bytes, "{carrier:?}");
    }
}

#[test]
fn refuses_to_overwrite_carrier_or_payload() {
    let dir = tempfile::tempdir().unwrap();
    let carrier = make_png(dir.path(), "picture.png");
    let payload_path = dir.path().join("data.bin");
    fs::write(&payload_path, b"data").unwrap();
    assert!(payload::embed_file(
        &carrier,
        &payload_path,
        &carrier,
        &ContainerOptions::default(),
        1,
        &Job::quiet(),
    )
    .is_err());
    assert!(payload::embed_file(
        &carrier,
        &payload_path,
        &payload_path,
        &ContainerOptions::default(),
        1,
        &Job::quiet(),
    )
    .is_err());
}

#[test]
fn detects_all_carrier_types() {
    let dir = tempfile::tempdir().unwrap();
    for (expected, path) in all_carriers(dir.path()) {
        assert_eq!(
            payload::detect_carrier(&path).unwrap(),
            expected,
            "{}",
            path.display()
        );
    }
    let html = dir.path().join("page.html");
    fs::write(&html, "<html><body><svg></svg></body></html>").unwrap();
    assert_eq!(payload::detect_carrier(&html).unwrap(), Carrier::Raw);
}

#[test]
fn image_carriers_stay_decodable_after_embedding() {
    let dir = tempfile::tempdir().unwrap();
    let payload_path = dir.path().join("secret.bin");
    fs::write(&payload_path, sample_payload()).unwrap();

    for (carrier, carrier_path) in all_carriers(dir.path()) {
        let extension = carrier_path
            .extension()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let output = with_extension(&dir.path().join(format!("decode-{carrier:?}")), &extension);
        payload::embed_file(
            &carrier_path,
            &payload_path,
            &output,
            &ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();

        match carrier {
            Carrier::Png | Carrier::Jpeg | Carrier::Gif => {
                assert!(image::open(&output).is_ok(), "{carrier:?} не открывается");
            }
            Carrier::Webp => {
                // Тестовый WebP собран вручную и не содержит настоящих видеоданных.
                assert!(fs::read(&output).unwrap().starts_with(b"RIFF"));
            }
            Carrier::Svg => {
                let text = fs::read_to_string(&output).unwrap();
                assert!(text.trim_end().ends_with("</svg>"));
                assert!(text.contains("<svg"));
            }
            Carrier::Raw => {}
        }
    }
}
