//! Режимы увеличения размера файла.

pub mod animation;
pub mod office;
pub mod padding;
pub mod png_chunks;
pub mod svg;
pub mod upscale;

use std::path::Path;

use crate::util::Job;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Animation,
    PngChunks,
    Upscale,
    Svg,
    OfficePdf,
    Padding,
}

impl Method {
    pub const ALL: [Method; 6] = [
        Method::Animation,
        Method::PngChunks,
        Method::Upscale,
        Method::Svg,
        Method::OfficePdf,
        Method::Padding,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Method::Animation => "GIF/APNG/WebP-анимация из изображения",
            Method::PngChunks => "Приватные PNG-чанки до нужного размера",
            Method::Upscale => "Увеличение изображения + случайные младшие биты",
            Method::Svg => "SVG со встроенным изображением и невидимыми элементами",
            Method::OfficePdf => "Office/ZIP и PDF; прочие файлы — добавление заполнения",
            Method::Padding => "Копия файла со случайным заполнением в конце",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AnimationFormat {
    #[default]
    Gif,
    Png,
    Webp,
}

impl AnimationFormat {
    pub fn extension(self) -> &'static str {
        match self {
            AnimationFormat::Gif => "gif",
            AnimationFormat::Png => "png",
            AnimationFormat::Webp => "webp",
        }
    }
}

pub fn run(
    method: Method,
    format: AnimationFormat,
    input: &Path,
    output: &Path,
    target_bytes: u64,
    job: &Job<'_>,
) -> Result<(), String> {
    match method {
        Method::Animation => animation::run(input, output, target_bytes, format, job),
        Method::PngChunks => png_chunks::run(input, output, target_bytes, job),
        Method::Upscale => upscale::run(input, output, target_bytes, job),
        Method::Svg => svg::run(input, output, target_bytes, job),
        Method::OfficePdf => office::run(input, output, target_bytes, job),
        Method::Padding => padding::run(input, output, target_bytes, job),
    }
}
