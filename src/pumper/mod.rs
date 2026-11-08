//! Режимы увеличения размера файла и создание файлов «с нуля».

pub mod animation;
pub mod generate;
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

/// Что создавать «с нуля», когда исходного файла нет вовсе.
///
/// Содержимое — криптослучайные байты, поэтому сжать результат архиватором
/// не получится. Картиночные типы остаются полностью валидными изображениями:
/// объём лежит внутри структуры файла (служебные чанки PNG, комментарий GIF),
/// а не после неё.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GeneratedKind {
    /// Обычный файл неизвестного формата (`.bin`).
    #[default]
    Bin,
    Png,
    Gif,
    Jpeg,
    Webp,
}

impl GeneratedKind {
    pub const ALL: [GeneratedKind; 5] = [
        GeneratedKind::Bin,
        GeneratedKind::Png,
        GeneratedKind::Gif,
        GeneratedKind::Jpeg,
        GeneratedKind::Webp,
    ];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "bin" | "file" | "dat" | "файл" => Ok(GeneratedKind::Bin),
            "png" => Ok(GeneratedKind::Png),
            "gif" => Ok(GeneratedKind::Gif),
            "jpg" | "jpeg" => Ok(GeneratedKind::Jpeg),
            "webp" => Ok(GeneratedKind::Webp),
            other => Err(format!(
                "Неизвестный тип файла: {other}. Допустимо: bin, png, gif, jpg, webp."
            )),
        }
    }

    /// Расширение созданного файла.
    pub fn extension(self) -> &'static str {
        match self {
            GeneratedKind::Bin => "bin",
            GeneratedKind::Png => "png",
            GeneratedKind::Gif => "gif",
            GeneratedKind::Jpeg => "jpg",
            GeneratedKind::Webp => "webp",
        }
    }

    /// Название для интерфейса и справки.
    pub fn title(self) -> &'static str {
        match self {
            GeneratedKind::Bin => "Обычный файл (.bin)",
            GeneratedKind::Png => "PNG-картинка (.png)",
            GeneratedKind::Gif => "GIF-картинка (.gif)",
            GeneratedKind::Jpeg => "JPEG-картинка (.jpg)",
            GeneratedKind::Webp => "WebP-картинка (.webp)",
        }
    }

    /// Картиночные типы — валидные изображения с «вторым дном» внутри структуры.
    pub fn is_image(self) -> bool {
        !matches!(self, GeneratedKind::Bin)
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
