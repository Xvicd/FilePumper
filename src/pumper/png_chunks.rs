//! Режим 2: любое изображение → PNG + приватные чанки до нужного размера.

use std::path::Path;

use crate::pumper::padding::append_png_padding;
use crate::util::Job;

pub fn run(input: &Path, output: &Path, target: u64, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    job.report(5.0, "Преобразование изображения в PNG...");
    image::open(input)
        .map_err(|err| format!("Не удалось открыть изображение: {err}"))?
        .to_rgb8()
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|err| format!("Не удалось сохранить PNG: {err}"))?;
    append_png_padding(output, target, job)
}
