//! Режим 1: анимация (GIF) либо сохранение в PNG/WebP с добивкой до размера.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use gif::{Encoder as GifEncoder, Frame as GifFrame, Repeat};
use rand::Rng;

use crate::pumper::padding::{append_png_padding, append_random_padding};
use crate::pumper::AnimationFormat;
use crate::util::Job;

pub fn run(
    input: &Path,
    output: &Path,
    target: u64,
    format: AnimationFormat,
    job: &Job<'_>,
) -> Result<(), String> {
    job.check()?;
    job.report(2.0, "Открытие изображения...");
    let image =
        image::open(input).map_err(|err| format!("Не удалось открыть изображение: {err}"))?;

    if format == AnimationFormat::Gif {
        let rgb = image.to_rgb8();
        let width = u16::try_from(rgb.width())
            .map_err(|_| "Ширина GIF не должна превышать 65535 пикселей".to_owned())?;
        let height = u16::try_from(rgb.height())
            .map_err(|_| "Высота GIF не должна превышать 65535 пикселей".to_owned())?;
        let mut file = File::create(output).map_err(|err| err.to_string())?;
        {
            let mut encoder = GifEncoder::new(&mut file, width, height, &[])
                .map_err(|err| format!("Не удалось создать GIF: {err}"))?;
            encoder
                .set_repeat(Repeat::Infinite)
                .map_err(|err| format!("Не удалось настроить GIF: {err}"))?;
            let base = rgb.into_raw();
            for frame_index in 0..76 {
                job.check()?;
                let mut pixels = base.clone();
                if frame_index > 0 {
                    let mut rng = rand::thread_rng();
                    for channel in &mut pixels {
                        *channel =
                            (*channel as i16 + rng.gen_range(-3_i16..=3_i16)).clamp(0, 255) as u8;
                    }
                }
                let mut frame = GifFrame::from_rgb_speed(width, height, &pixels, 10);
                frame.delay = 8;
                encoder
                    .write_frame(&frame)
                    .map_err(|err| format!("Не удалось записать GIF-кадр: {err}"))?;
                if frame_index % 8 == 0 {
                    job.report(
                        5.0 + frame_index as f32 / 76.0 * 80.0,
                        "Создание кадров GIF-анимации...",
                    );
                }
            }
        }
        file.flush().map_err(|err| err.to_string())?;
    } else {
        let image_format = match format {
            AnimationFormat::Png => image::ImageFormat::Png,
            AnimationFormat::Webp => image::ImageFormat::WebP,
            AnimationFormat::Gif => unreachable!(),
        };
        job.report(40.0, "Сохранение изображения...");
        image
            .save_with_format(output, image_format)
            .map_err(|err| format!("Не удалось сохранить изображение: {err}"))?;
    }

    job.check()?;
    let size = fs::metadata(output).map_err(|err| err.to_string())?.len();
    if size < target {
        match format {
            AnimationFormat::Png => append_png_padding(output, target, job)?,
            AnimationFormat::Gif => {
                crate::pumper::padding::append_gif_comment_padding(output, target, job)?
            }
            AnimationFormat::Webp => append_random_padding(output, target, job)?,
        }
    }
    Ok(())
}
