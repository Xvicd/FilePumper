//! Режим 3: увеличение изображения до нужного числа пикселей и шум в младших
//! битах — размер растёт за счёт самих данных, без хвостов-заполнителей.

use std::fs::{self, File};
use std::path::Path;

use image::ImageEncoder;
use rand::Rng;

use crate::pumper::padding::append_png_padding;
use crate::util::Job;

pub fn run(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    job.report(5.0, "Расчёт размера изображения...");
    let image = image::open(input)
        .map_err(|err| format!("Не удалось открыть изображение: {err}"))?
        .to_rgb8();
    let side = ((target / 3) as f64).sqrt().floor().clamp(1.0, 15_000.0) as u32;
    job.check()?;
    job.report(
        20.0,
        &format!("Масштабирование до {side}x{side} пикселей..."),
    );
    let resized = image::imageops::resize(&image, side, side, image::imageops::FilterType::Nearest);
    let mut pixels = resized.into_raw();
    job.check()?;
    job.report(50.0, "Добавление случайных младших битов...");
    let mut rng = rand::thread_rng();
    for channel in &mut pixels {
        *channel = (*channel & 0xFC) | rng.gen_range(0..4);
    }

    job.check()?;
    job.report(75.0, "Сохранение PNG...");
    let file = File::create(output).map_err(|err| err.to_string())?;
    image::codecs::png::PngEncoder::new_with_quality(
        file,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(&pixels, side, side, image::ColorType::Rgb8.into())
    .map_err(|err| format!("Не удалось записать PNG: {err}"))?;

    job.check()?;
    if fs::metadata(output).map_err(|err| err.to_string())?.len() < target {
        append_png_padding(output, target, job)?;
    }
    Ok(())
}
