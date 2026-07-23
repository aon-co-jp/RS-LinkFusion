//! トンネルペイロードの圧縮+暗号化ハードウェアアクセラレータ抽象化。
//!
//! `open-web-server-wire::accel`と同じ設計判断(CPUのみ実装、GPU/NPU/
//! 専用ハードウェアアクセラレータは安全にフォールバックする未実装の
//! 拡張点)を、本リポジトリが独立した配布物であることを踏まえて
//! 自己完結で再実装したもの(open-web-serverへの依存を持たせず、
//! ダウンロード後すぐ動く単体バイナリにするため)。

use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelBackend {
    /// 実装済み。`flate2`(deflate)圧縮+ChaCha20-Poly1305 AEAD暗号化。
    Cpu,
    /// 未実装の拡張点(NVIDIA nvCOMP等によるGPU圧縮・GPU暗号化は実在
    /// 技術として存在するが本クレートには未統合)。
    Gpu,
    /// 未実装の拡張点。
    Npu,
    /// 未実装の拡張点(専用暗号化アクセラレータカード等)。
    HardwareAccelerator,
}

pub struct PayloadAccelerator {
    backend: AccelBackend,
    cipher: ChaCha20Poly1305,
}

impl PayloadAccelerator {
    pub fn new(backend: AccelBackend, key: &[u8; 32]) -> Self {
        let effective = match backend {
            AccelBackend::Cpu => AccelBackend::Cpu,
            other => {
                tracing::warn!(requested = ?other, "accelerator backend not yet implemented, falling back to Cpu");
                AccelBackend::Cpu
            }
        };
        Self { backend: effective, cipher: ChaCha20Poly1305::new(Key::from_slice(key)) }
    }

    pub fn generate_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        key
    }

    pub fn backend(&self) -> AccelBackend {
        self.backend
    }

    /// 平文を圧縮してから暗号化し、`nonce || ciphertext`を返す。
    pub fn seal(&self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let compressed = deflate_compress(plaintext)?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext =
            self.cipher.encrypt(nonce, compressed.as_slice()).map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// `nonce || ciphertext`から復号してから解凍する。
    pub fn open(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        if data.len() < 12 {
            anyhow::bail!("frame too short to contain nonce");
        }
        let (nonce_bytes, ciphertext) = data.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let compressed =
            self.cipher.decrypt(nonce, ciphertext).map_err(|e| anyhow::anyhow!("decrypt failed (tamper detected?): {e}"))?;
        deflate_decompress(&compressed)
    }
}

fn deflate_compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn deflate_decompress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_backend_round_trips_and_actually_compresses() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Cpu, &key);
        let plaintext = b"tunnel payload tunnel payload tunnel payload ".repeat(50);
        let sealed = accel.seal(&plaintext).unwrap();
        assert!(sealed.len() < plaintext.len());
        assert_eq!(accel.open(&sealed).unwrap(), plaintext);
    }

    #[test]
    fn unimplemented_backend_falls_back_to_cpu() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Gpu, &key);
        assert_eq!(accel.backend(), AccelBackend::Cpu);
        let sealed = accel.seal(b"still works").unwrap();
        assert_eq!(accel.open(&sealed).unwrap(), b"still works");
    }

    #[test]
    fn tampered_frame_is_rejected() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Cpu, &key);
        let mut sealed = accel.seal(b"secret tunnel data").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(accel.open(&sealed).is_err());
    }
}
