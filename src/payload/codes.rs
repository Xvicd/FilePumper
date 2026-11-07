//! Сжатие (deflate) и шифрование (Argon2id + ChaCha20-Poly1305) полезной нагрузки.

use std::io::{Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const KEY_LEN: usize = 32;
pub const TAG_LEN: usize = 16;

/// Параметры Argon2id по умолчанию: 19 МиБ памяти, 2 прохода.
pub const DEFAULT_M_COST: u32 = 19_456;
pub const DEFAULT_T_COST: u32 = 2;
pub const DEFAULT_P_COST: u32 = 1;
/// Границы, за которые нельзя выходить при чтении параметров из файла:
/// иначе чужой файл может потребовать гигабайты памяти.
pub const MIN_M_COST: u32 = 8 * 1024;
pub const MAX_M_COST: u32 = 262_144;
pub const MAX_T_COST: u32 = 10;
pub const MAX_P_COST: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CipherParams {
    pub salt: [u8; SALT_LEN],
    pub nonce: [u8; NONCE_LEN],
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl CipherParams {
    pub fn validate(&self) -> Result<(), String> {
        let broken =
            || "Полезная нагрузка повреждена: недопустимые параметры шифрования.".to_owned();
        if !(MIN_M_COST..=MAX_M_COST).contains(&self.m_cost) {
            return Err(broken());
        }
        if self.t_cost == 0 || self.t_cost > MAX_T_COST {
            return Err(broken());
        }
        if self.p_cost == 0 || self.p_cost > MAX_P_COST {
            return Err(broken());
        }
        Ok(())
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
    let mut result = [0_u8; 32];
    result.copy_from_slice(&digest);
    result
}

pub fn compress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(6));
    encoder
        .write_all(data)
        .map_err(|err| format!("Ошибка сжатия: {err}"))?;
    encoder
        .finish()
        .map_err(|err| format!("Ошибка сжатия: {err}"))
}

pub fn decompress(data: &[u8], expected_len: usize) -> Result<Vec<u8>, String> {
    let reader = DeflateDecoder::new(data);
    let mut limited = reader.take(expected_len as u64 + 1);
    let mut output = Vec::with_capacity(expected_len.min(8 * 1024 * 1024));
    limited
        .read_to_end(&mut output)
        .map_err(|err| format!("Полезная нагрузка повреждена: ошибка распаковки ({err})."))?;
    if output.len() != expected_len {
        return Err(
            "Полезная нагрузка повреждена: распакованный размер не совпадает с ожидаемым.".into(),
        );
    }
    Ok(output)
}

fn derive_key(password: &str, params: &CipherParams) -> Result<[u8; KEY_LEN], String> {
    let argon_params = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|err| format!("Ошибка параметров шифрования: {err}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = [0_u8; KEY_LEN];
    argon2
        .hash_password_into(password.as_bytes(), &params.salt, &mut key)
        .map_err(|err| format!("Ошибка вычисления ключа: {err}"))?;
    Ok(key)
}

pub fn encrypt(data: &[u8], password: &str) -> Result<(Vec<u8>, CipherParams), String> {
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::getrandom(&mut salt).map_err(|err| format!("Ошибка генерации соли: {err}"))?;
    getrandom::getrandom(&mut nonce).map_err(|err| format!("Ошибка генерации nonce: {err}"))?;
    let params = CipherParams {
        salt,
        nonce,
        m_cost: DEFAULT_M_COST,
        t_cost: DEFAULT_T_COST,
        p_cost: DEFAULT_P_COST,
    };
    let key = derive_key(password, &params)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&params.nonce), data)
        .map_err(|_| "Не удалось зашифровать полезную нагрузку.".to_owned())?;
    Ok((ciphertext, params))
}

pub fn decrypt(data: &[u8], password: &str, params: &CipherParams) -> Result<Vec<u8>, String> {
    params.validate()?;
    if data.len() < TAG_LEN {
        return Err("Полезная нагрузка повреждена: слишком короткий шифротекст.".into());
    }
    let key = derive_key(password, params)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(&params.nonce), data)
        .map_err(|_| "Неверный пароль или повреждённая полезная нагрузка.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_round_trip_shrinks_repeats() {
        let data = vec![b'A'; 100_000];
        let packed = compress(&data).unwrap();
        assert!(packed.len() < data.len());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
    }

    #[test]
    fn decompression_rejects_wrong_size() {
        let packed = compress(b"short").unwrap();
        assert!(decompress(&packed, 4).is_err());
    }

    #[test]
    fn encryption_round_trip_and_wrong_password() {
        let data = b"secret payload".repeat(100);
        let (ciphertext, params) = encrypt(&data, "hunter2").unwrap();
        assert_ne!(ciphertext, data.to_vec());
        assert_eq!(decrypt(&ciphertext, "hunter2", &params).unwrap(), data);
        assert!(decrypt(&ciphertext, "wrong", &params).is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (mut ciphertext, params) = encrypt(b"payload", "pw").unwrap();
        ciphertext[0] ^= 0xFF;
        assert!(decrypt(&ciphertext, "pw", &params).is_err());
    }

    #[test]
    fn absurd_parameters_are_rejected_before_hashing() {
        let mut params = CipherParams {
            salt: [7; SALT_LEN],
            nonce: [9; NONCE_LEN],
            m_cost: u32::MAX,
            t_cost: 2,
            p_cost: 1,
        };
        assert!(params.validate().is_err());
        params.m_cost = DEFAULT_M_COST;
        assert!(params.validate().is_ok());
    }

    #[test]
    fn sha256_matches_known_vector() {
        let digest = sha256(b"abc");
        assert_eq!(
            crate::util::hex_encode(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
