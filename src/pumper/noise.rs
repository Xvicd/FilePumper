//! Виды «шумовой» картинки: от неё зависит, как выглядит созданный файл.

use crate::pumper::generate::ImageSize;
use crate::util::random_bytes;

/// Какой шум рисуется в видимой части файла.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoiseKind {
    /// Цветной снег: каждый пиксель — случайный цвет.
    #[default]
    RgbSnow,
    /// Плавный градиент между двумя случайными цветами.
    Gradient,
    /// Шахматка из двух случайных цветов.
    Checker,
    /// «Телевизионный снег»: чёрно-белые точки с редкими цветными.
    TvSnow,
    /// Плавные пятна в духе шума Перлина.
    Perlin,
}

impl NoiseKind {
    pub const ALL: [NoiseKind; 5] = [
        NoiseKind::RgbSnow,
        NoiseKind::Gradient,
        NoiseKind::Checker,
        NoiseKind::TvSnow,
        NoiseKind::Perlin,
    ];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rgb" | "snow" | "rgbsnow" | "снег" | "цветной" => Ok(NoiseKind::RgbSnow),
            "gradient" | "градиент" => Ok(NoiseKind::Gradient),
            "checker" | "checkerboard" | "шахматка" | "шахматы" => {
                Ok(NoiseKind::Checker)
            }
            "tv" | "tvsnow" | "телевизор" | "тв" => Ok(NoiseKind::TvSnow),
            "perlin" | "перлин" | "пятна" => Ok(NoiseKind::Perlin),
            other => Err(format!(
                "Неизвестный вид шума: {other}. Допустимо: rgb, gradient, checker, tv, perlin."
            )),
        }
    }

    /// Короткое название для интерфейса.
    pub fn label(self) -> &'static str {
        match self {
            NoiseKind::RgbSnow => "цветной снег",
            NoiseKind::Gradient => "градиент",
            NoiseKind::Checker => "шахматка",
            NoiseKind::TvSnow => "телевизионный снег",
            NoiseKind::Perlin => "плавные пятна",
        }
    }

    /// Пояснение с примером.
    pub fn title(self) -> &'static str {
        match self {
            NoiseKind::RgbSnow => "Цветной снег: каждый пиксель случайного цвета",
            NoiseKind::Gradient => "Градиент: плавный переход между двумя случайными цветами",
            NoiseKind::Checker => "Шахматка: клетки двух случайных цветов",
            NoiseKind::TvSnow => "Телевизионный снег: светлые и тёмные точки с редкими цветными",
            NoiseKind::Perlin => "Плавные пятна: мягкий шум, похожий на облака",
        }
    }
}

/// RGB-байты шумовой картинки нужного размера.
pub fn pixels(kind: NoiseKind, size: ImageSize) -> Result<Vec<u8>, String> {
    let size = size.sanitized();
    let width = size.width as usize;
    let height = size.height as usize;
    let count = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(3))
        .ok_or_else(|| "Слишком большая картинка.".to_owned())?;
    match kind {
        NoiseKind::RgbSnow => random_bytes(count),
        NoiseKind::TvSnow => tv_snow(width, height),
        NoiseKind::Gradient => gradient(width, height),
        NoiseKind::Checker => checker(width, height),
        NoiseKind::Perlin => perlin(width, height),
    }
}

fn tv_snow(width: usize, height: usize) -> Result<Vec<u8>, String> {
    let raw = random_bytes(width * height * 3)?;
    let mut pixels = raw;
    for pixel in pixels.as_chunks_mut::<3>().0 {
        let level = pixel[0];
        if pixel[1] < 6 {
            // Редкий цветной пиксель — как «цветной шум» на старом телевизоре.
            continue;
        }
        let value = match level % 4 {
            0 => 0,
            1 => 255,
            _ => level,
        };
        pixel.copy_from_slice(&[value, value, value]);
    }
    Ok(pixels)
}

fn gradient(width: usize, height: usize) -> Result<Vec<u8>, String> {
    let colors = random_bytes(6)?;
    let (first, second) = (
        [colors[0], colors[1], colors[2]],
        [colors[3], colors[4], colors[5]],
    );
    let diagonal = colors[5] & 1 == 0;
    let span = if diagonal {
        (width + height).saturating_sub(2).max(1) as f32
    } else {
        width.saturating_sub(1).max(1) as f32
    };
    let mut pixels = vec![0_u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let position = if diagonal { x + y } else { x } as f32;
            let mix = (position / span).clamp(0.0, 1.0);
            let index = (y * width + x) * 3;
            for channel in 0..3 {
                let value = first[channel] as f32 * (1.0 - mix) + second[channel] as f32 * mix;
                pixels[index + channel] = value.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(pixels)
}

fn checker(width: usize, height: usize) -> Result<Vec<u8>, String> {
    let colors = random_bytes(6)?;
    let (first, second) = (
        [colors[0], colors[1], colors[2]],
        [colors[3], colors[4], colors[5]],
    );
    let cell = (width.min(height) / 8).clamp(2, 256);
    let mut pixels = vec![0_u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let color = if (x / cell + y / cell).is_multiple_of(2) {
                first
            } else {
                second
            };
            let index = (y * width + x) * 3;
            pixels[index..index + 3].copy_from_slice(&color);
        }
    }
    Ok(pixels)
}

fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

/// Плавный шум: случайная сетка, растянутая билинейной интерполяцией.
fn perlin(width: usize, height: usize) -> Result<Vec<u8>, String> {
    let cells = (width.min(height) / 64).clamp(4, 24);
    let side = cells + 1;
    let raw = random_bytes(side * side * 3)?;
    let grid: Vec<[f32; 3]> = raw
        .as_chunks::<3>()
        .0
        .iter()
        .map(|chunk| {
            [
                chunk[0] as f32 / 255.0,
                chunk[1] as f32 / 255.0,
                chunk[2] as f32 / 255.0,
            ]
        })
        .collect();
    let step_x = cells as f32 / width.saturating_sub(1).max(1) as f32;
    let step_y = cells as f32 / height.saturating_sub(1).max(1) as f32;
    let mut pixels = vec![0_u8; width * height * 3];
    for y in 0..height {
        let fy = y as f32 * step_y;
        let y0 = (fy.floor() as usize).min(side - 2);
        let ty = smoothstep(fy - y0 as f32);
        for x in 0..width {
            let fx = x as f32 * step_x;
            let x0 = (fx.floor() as usize).min(side - 2);
            let tx = smoothstep(fx - x0 as f32);
            let index = (y * width + x) * 3;
            for channel in 0..3 {
                let top_left = grid[y0 * side + x0][channel];
                let top_right = grid[y0 * side + x0 + 1][channel];
                let bottom_left = grid[(y0 + 1) * side + x0][channel];
                let bottom_right = grid[(y0 + 1) * side + x0 + 1][channel];
                let top = top_left * (1.0 - tx) + top_right * tx;
                let bottom = bottom_left * (1.0 - tx) + bottom_right * tx;
                let value = top * (1.0 - ty) + bottom * ty;
                pixels[index + channel] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_noise_kind_produces_pixels_of_the_right_size() {
        let size = ImageSize {
            width: 32,
            height: 24,
        };
        for kind in NoiseKind::ALL {
            let pixels = pixels(kind, size).unwrap();
            assert_eq!(pixels.len(), 32 * 24 * 3, "{}", kind.label());
            // Картинка не однотонная: у градиента и шахматки тоже есть разброс.
            let first = &pixels[..3];
            let different = pixels
                .as_chunks::<3>()
                .0
                .iter()
                .any(|pixel| pixel.as_slice() != first);
            assert!(different, "{}: однотонная картинка", kind.label());
        }
    }

    #[test]
    fn noise_kind_parses_aliases() {
        assert_eq!(NoiseKind::parse("rgb").unwrap(), NoiseKind::RgbSnow);
        assert_eq!(NoiseKind::parse("градиент").unwrap(), NoiseKind::Gradient);
        assert_eq!(NoiseKind::parse("checker").unwrap(), NoiseKind::Checker);
        assert_eq!(NoiseKind::parse("tv").unwrap(), NoiseKind::TvSnow);
        assert_eq!(NoiseKind::parse("perlin").unwrap(), NoiseKind::Perlin);
        assert!(NoiseKind::parse("random").is_err());
    }

    #[test]
    fn checker_is_made_of_two_colors() {
        let size = ImageSize {
            width: 64,
            height: 64,
        };
        let pixels = pixels(NoiseKind::Checker, size).unwrap();
        let colors: std::collections::HashSet<&[u8]> = pixels
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| pixel.as_slice())
            .collect();
        assert_eq!(colors.len(), 2, "{:?}", colors);
    }
}
