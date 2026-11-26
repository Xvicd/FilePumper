//! Разделение секрета по схеме Шамира над GF(256).
//!
//! Секрет режется на `total` частей так, что любые `threshold` из них
//! восстанавливают исходник, а меньшее число не даёт о нём ничего. Каждая
//! часть — обычный файл с заголовком: индекс, порог, всего частей и длина.

use std::path::Path;

use crate::util::random_bytes;

pub const SHARE_MAGIC: &[u8; 4] = b"FSHR";
pub const SHARE_VERSION: u8 = 1;
/// magic(4) + version(1) + index(1) + threshold(1) + total(1) + len(8)
pub const SHARE_HEADER_LEN: usize = 16;
/// Больше частей не имеет смысла: индексы — байты GF(256).
pub const MAX_SHARES: u8 = 255;

/// Умножение в GF(256) с полиномом 0x11B (как в AES).
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut result = 0_u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            result ^= a;
        }
        let high = a & 0x80 != 0;
        a <<= 1;
        if high {
            a ^= 0x1B;
        }
        b >>= 1;
    }
    result
}

fn gf_pow(mut base: u8, mut exponent: u8) -> u8 {
    let mut result = 1_u8;
    while exponent > 0 {
        if exponent & 1 != 0 {
            result = gf_mul(result, base);
        }
        base = gf_mul(base, base);
        exponent >>= 1;
    }
    result
}

/// Обратный элемент: `a^(254)` в GF(256); для нуля не существует.
fn gf_inv(a: u8) -> Result<u8, String> {
    if a == 0 {
        return Err("В частях секрета повторяются индексы.".to_owned());
    }
    Ok(gf_pow(a, 254))
}

/// Значение многочлена в точке `x` (схема Горнера).
fn evaluate(coefficients: &[u8], x: u8) -> u8 {
    let mut value = 0_u8;
    for coefficient in coefficients.iter().rev() {
        value = gf_mul(value, x) ^ coefficient;
    }
    value
}

/// Части секрета: у каждой первый байт — её номер (1…255), дальше данные.
pub fn split(secret: &[u8], threshold: u8, total: u8) -> Result<Vec<Vec<u8>>, String> {
    if !(2..=MAX_SHARES).contains(&total) {
        return Err(format!("Частей должно быть от 2 до {MAX_SHARES}."));
    }
    if threshold == 0 || threshold > total {
        return Err(format!(
            "Порог должен быть от 1 до {total}: столько частей нужно для сборки."
        ));
    }
    if secret.is_empty() {
        return Err("Пустой файл делить не на что.".to_owned());
    }
    let random = random_bytes(secret.len() * (threshold as usize - 1))?;
    let mut shares: Vec<Vec<u8>> = (1..=total).map(|index| vec![index]).collect();
    for (position, byte) in secret.iter().enumerate() {
        let mut coefficients = Vec::with_capacity(threshold as usize);
        coefficients.push(*byte);
        let start = position * (threshold as usize - 1);
        coefficients.extend_from_slice(&random[start..start + threshold as usize - 1]);
        for share in shares.iter_mut() {
            let x = share[0];
            share.push(evaluate(&coefficients, x));
        }
    }
    Ok(shares)
}

/// Собирает секрет из частей; нужны хотя бы `threshold` разных частей.
pub fn combine(shares: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if shares.is_empty() {
        return Err("Не указана ни одна часть секрета.".to_owned());
    }
    let mut parsed = Vec::with_capacity(shares.len());
    let mut threshold = 0_u8;
    let mut total = 0_u8;
    let mut length = 0_usize;
    for bytes in shares {
        let share = parse_share(bytes)?;
        if threshold == 0 {
            threshold = share.threshold;
            total = share.total;
            length = share.data.len();
        }
        if share.threshold != threshold || share.total != total || share.data.len() != length {
            return Err("Части секрета от разных файлов или повреждены.".to_owned());
        }
        parsed.push(share);
    }
    if parsed.len() < threshold as usize {
        return Err(format!(
            "Нужно минимум {threshold} частей, а передано {}.",
            parsed.len()
        ));
    }
    parsed.sort_by_key(|share| share.index);
    for window in parsed.windows(2) {
        if window[0].index == window[1].index {
            return Err("Части секрета повторяются.".to_owned());
        }
    }
    parsed.truncate(threshold as usize);

    let mut secret = Vec::with_capacity(length);
    for position in 0..length {
        let mut value = 0_u8;
        for (i, current) in parsed.iter().enumerate() {
            let x = current.index;
            let mut numerator = 1_u8;
            let mut denominator = 1_u8;
            for (j, other) in parsed.iter().enumerate() {
                if i == j {
                    continue;
                }
                numerator = gf_mul(numerator, other.index);
                denominator = gf_mul(denominator, x ^ other.index);
            }
            value ^= gf_mul(
                current.data[position],
                gf_mul(numerator, gf_inv(denominator)?),
            );
        }
        secret.push(value);
    }
    Ok(secret)
}

/// Часть секрета в виде файла: заголовок и данные.
pub fn pack_share(share: &[u8], threshold: u8, total: u8) -> Result<Vec<u8>, String> {
    if share.is_empty() {
        return Err("Пустая часть секрета.".to_owned());
    }
    let index = share[0];
    let data = &share[1..];
    let mut bytes = Vec::with_capacity(SHARE_HEADER_LEN + data.len());
    bytes.extend_from_slice(SHARE_MAGIC);
    bytes.push(SHARE_VERSION);
    bytes.push(index);
    bytes.push(threshold);
    bytes.push(total);
    bytes.extend_from_slice(&(data.len() as u64).to_be_bytes());
    bytes.extend_from_slice(data);
    Ok(bytes)
}

/// Разобранная часть секрета.
#[derive(Debug)]
pub struct ParsedShare {
    pub index: u8,
    pub threshold: u8,
    pub total: u8,
    pub data: Vec<u8>,
}

pub fn parse_share(bytes: &[u8]) -> Result<ParsedShare, String> {
    if bytes.len() < SHARE_HEADER_LEN || &bytes[..4] != SHARE_MAGIC {
        return Err("Это не часть секрета FilePumper.".to_owned());
    }
    if bytes[4] != SHARE_VERSION {
        return Err(format!(
            "Часть секрета сделана другой версией ({}).",
            bytes[4]
        ));
    }
    let index = bytes[5];
    let threshold = bytes[6];
    let total = bytes[7];
    if index == 0 || index > total || threshold == 0 || threshold > total {
        return Err("Часть секрета повреждена: неверные индекс или порог.".to_owned());
    }
    let mut length_bytes = [0_u8; 8];
    length_bytes.copy_from_slice(&bytes[8..16]);
    let length = u64::from_be_bytes(length_bytes);
    let length = usize::try_from(length).map_err(|_| "Часть секрета слишком велика.".to_owned())?;
    if bytes.len() != SHARE_HEADER_LEN + length {
        return Err("Часть секрета обрезана или повреждена.".to_owned());
    }
    Ok(ParsedShare {
        index,
        threshold,
        total,
        data: bytes[SHARE_HEADER_LEN..].to_vec(),
    })
}

/// Имя файла части: `secret.zip` → `secret.zip.share1`.
pub fn share_path(path: &Path, index: u8) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("secret");
    path.with_file_name(format!("{name}.share{index}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packed(share: &[u8], threshold: u8, total: u8) -> Vec<u8> {
        pack_share(share, threshold, total).unwrap()
    }

    #[test]
    fn any_threshold_shares_restore_the_secret() {
        let secret = b"sensitive payload material".to_vec();
        let shares = split(&secret, 3, 5).unwrap();
        assert_eq!(shares.len(), 5);
        for first in 0..shares.len() {
            for second in first + 1..shares.len() {
                for third in second + 1..shares.len() {
                    let combined = combine(&[
                        packed(&shares[first], 3, 5),
                        packed(&shares[second], 3, 5),
                        packed(&shares[third], 3, 5),
                    ])
                    .unwrap();
                    assert_eq!(combined, secret, "набор {first}-{second}-{third}");
                }
            }
        }
        let error = combine(&[packed(&shares[0], 3, 5), packed(&shares[1], 3, 5)]).unwrap_err();
        assert!(error.contains("минимум 3"), "{error}");
    }

    #[test]
    fn shares_can_be_packed_and_parsed() {
        let shares = split(b"hello world", 2, 3).unwrap();
        let bytes = packed(&shares[1], 2, 3);
        let parsed = parse_share(&bytes).unwrap();
        assert_eq!(parsed.index, shares[1][0]);
        assert_eq!(parsed.threshold, 2);
        assert_eq!(parsed.total, 3);
        assert_eq!(parsed.data, shares[1][1..].to_vec());
        assert_eq!(
            combine(&[packed(&shares[0], 2, 3), packed(&shares[1], 2, 3)]).unwrap(),
            b"hello world"
        );
    }

    #[test]
    fn broken_share_is_rejected() {
        let shares = split(b"hello", 2, 3).unwrap();
        let mut bytes = packed(&shares[0], 2, 3);
        bytes[5] = 0;
        assert!(parse_share(&bytes).is_err());
        let mut bytes = packed(&shares[1], 2, 3);
        bytes.truncate(bytes.len() - 1);
        assert!(parse_share(&bytes).is_err());
        assert!(parse_share(b"not a share").is_err());
    }

    #[test]
    fn one_share_reveals_nothing() {
        let secret = b"one-byte".to_vec();
        let shares = split(&secret, 3, 3).unwrap();
        for share in &shares {
            assert_ne!(&share[1..], secret.as_slice());
        }
    }

    #[test]
    fn absurd_parameters_are_rejected() {
        assert!(split(b"x", 4, 3).is_err());
        assert!(split(b"x", 1, 1).is_err());
        assert!(split(b"x", 0, 3).is_err());
        assert!(split(b"", 2, 3).is_err());
    }
}
