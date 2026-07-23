//! トンネルペイロードの圧縮+暗号化ハードウェアアクセラレータ抽象化。
//!
//! CPU バックエンドは flate2 + ChaCha20-Poly1305 (AEAD)。
//! GPU バックエンドは opencuda-directx の ChaCha20 カーネル(暗号化のみ、
//! 圧縮はCPU)+ **CPU側で計算するRFC 8439準拠のPoly1305認証タグ**を
//! 組み合わせる。
//!
//! **正直な開示・実バグの修正(2026-07-23)**: 当初のGPU実装は
//! ChaCha20のみでPoly1305認証タグを計算しておらず、GPUバックエンド
//! 選択時に改ざん検知が効かない(`open()`が改ざんされたデータを
//! そのまま受理してしまう)という実質的な脆弱性があった
//! (`tampered_frame_is_rejected`テストがCPUパスしか検証していな
//! かったため見過ごされていた)。RFC 8439のAEAD構成(counter=0の
//! ブロックからPoly1305一時鍵を導出し、実データ暗号化はcounter=1
//! から開始、`ciphertext || pad16 || aad_len || ciphertext_len`を
//! MACする)をCPU側`poly1305`クレートで実装し、GPU(ChaCha20暗号化)
//! と組み合わせることで、GPUバックエンドでもCPUバックエンドと
//! 同等の改ざん耐性を持たせた。**この構成が`chacha20poly1305`
//! クレートの出力と完全一致することをテストで検証済み**
//! (`gpu_poly1305_construction_matches_chacha20poly1305_reference`)。

use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;
use std::io::Read;
use std::sync::Arc;
use subtle::ConstantTimeEq;

// ===== GPU サポート(open-cuda依存) =====
use opencuda_core::{CompiledKernel, GpuDevice, KernelArg, LaunchConfig};

#[cfg(feature = "gpu")]
use opencuda_directx::DirectXDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelBackend {
    /// 実装済み。`flate2`(deflate)圧縮+ChaCha20-Poly1305 AEAD暗号化（CPU）。
    Cpu,
    /// 実装済み（Windows + gpu feature 時）。`opencuda-directx` の ChaCha20 カーネルを使用。
    Gpu,
    /// 未実装の拡張点。
    Npu,
    /// 未実装の拡張点。
    HardwareAccelerator,
}

pub struct PayloadAccelerator {
    backend: AccelBackend,
    cipher: ChaCha20Poly1305,
    /// GPU デバイス（Gpu バックエンドで使用）
    gpu_device: Option<Arc<dyn GpuDevice>>,
    /// コンパイル済み ChaCha20 カーネル（遅延ロード）
    chacha20_kernel: Option<CompiledKernel>,
    /// GPU カーネル用の鍵（32バイト）
    gpu_key: [u8; 32],
}

impl PayloadAccelerator {
    pub fn new(backend: AccelBackend, key: &[u8; 32]) -> Self {
        let mut accel = Self {
            backend: AccelBackend::Cpu,
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            gpu_device: None,
            chacha20_kernel: None,
            gpu_key: *key,
        };

        // GPU が要求された場合のみ試行
        if backend == AccelBackend::Gpu {
            #[cfg(feature = "gpu")]
            {
                if let Ok(dev) = DirectXDevice::new(0) {
                    let dev: Arc<dyn GpuDevice> = dev;
                    if let Some(kernel) = load_chacha20_kernel() {
                        accel.backend = AccelBackend::Gpu;
                        accel.gpu_device = Some(dev);
                        accel.chacha20_kernel = Some(kernel);
                        tracing::info!("GPU backend initialized with ChaCha20 kernel");
                    } else {
                        tracing::warn!("GPU device OK but kernel load failed, falling back to Cpu");
                    }
                } else {
                    tracing::warn!("GPU device init failed, falling back to Cpu");
                }
            }
            #[cfg(not(feature = "gpu"))]
            {
                tracing::warn!("GPU feature not enabled, falling back to Cpu");
            }
        }

        accel
    }

    pub fn generate_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        key
    }

    pub fn backend(&self) -> AccelBackend {
        self.backend
    }

    /// 平文を圧縮してから暗号化し、`nonce || ciphertext` を返す。
    pub fn seal(&self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let compressed = deflate_compress(plaintext)?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

        let ciphertext = match self.backend {
            AccelBackend::Gpu => {
                let dev = self.gpu_device.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("GPU backend selected but device not available")
                })?;
                let kernel = self.chacha20_kernel.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("ChaCha20 GPU kernel not loaded")
                })?;

                let ciphertext = gpu_chacha20_encrypt(dev, kernel, &self.gpu_key, &nonce_bytes, &compressed)?;
                let tag = poly1305_tag(&self.gpu_key, &nonce_bytes, &ciphertext);

                let mut out = ciphertext;
                out.extend_from_slice(&tag);
                out
            }
            AccelBackend::Cpu => {
                let nonce = Nonce::from_slice(&nonce_bytes);
                self.cipher
                    .encrypt(nonce, compressed.as_slice())
                    .map_err(|e| anyhow::anyhow!("CPU encrypt failed: {}", e))?
            }
            _ => unreachable!(),
        };

        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// `nonce || ciphertext` から復号して解凍する。
    pub fn open(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        if data.len() < 12 {
            anyhow::bail!("frame too short to contain nonce");
        }
        let (nonce_bytes, ciphertext) = data.split_at(12);

        let compressed = match self.backend {
            AccelBackend::Gpu => {
                let dev = self.gpu_device.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("GPU backend selected but device not available")
                })?;
                let kernel = self.chacha20_kernel.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("ChaCha20 GPU kernel not loaded")
                })?;

                if ciphertext.len() < 16 {
                    anyhow::bail!("GPU frame too short to contain a Poly1305 tag");
                }
                let (ct, received_tag) = ciphertext.split_at(ciphertext.len() - 16);

                // 検証してから復号する(verify-then-decrypt)。改ざんされた
                // データをGPUカーネルへ渡す前にここで確実に拒否する。
                let expected_tag = poly1305_tag(&self.gpu_key, nonce_bytes, ct);
                if expected_tag.ct_eq(received_tag).unwrap_u8() != 1 {
                    anyhow::bail!("GPU frame authentication failed (tamper detected)");
                }

                gpu_chacha20_encrypt(dev, kernel, &self.gpu_key, nonce_bytes, ct)?
            }
            AccelBackend::Cpu => {
                let nonce = Nonce::from_slice(nonce_bytes);
                self.cipher
                    .decrypt(nonce, ciphertext)
                    .map_err(|e| anyhow::anyhow!("CPU decrypt failed (tamper detected?): {}", e))?
            }
            _ => unreachable!(),
        };

        deflate_decompress(&compressed)
    }
}

// ===== 圧縮・解凍（CPU） =====
fn deflate_compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
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

// ===== GPU カーネルロード =====
fn load_chacha20_kernel() -> Option<CompiledKernel> {
    let path = match std::env::var("CHA_CHA20_DXIL_PATH") {
        Ok(p) => p,
        Err(_) => {
            // デフォルトパス（実行ファイルからの相対パス）
            let exe = std::env::current_exe().ok()?;
            let parent = exe.parent()?;
            let path = parent.join("shaders/chacha20.dxil");
            path.to_str()?.to_string()
        }
    };
    std::fs::read(&path).ok().map(|bytes| CompiledKernel::dxil("chacha20", "main", bytes))
}

// ===== 補助関数 =====
fn to_u32_array_le<const N: usize>(bytes: &[u8]) -> anyhow::Result<[u32; N]> {
    if bytes.len() < N * 4 {
        anyhow::bail!("byte slice too short for {} u32s", N);
    }
    let mut out = [0u32; N];
    for (i, chunk) in bytes.chunks_exact(4).take(N).enumerate() {
        out[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(out)
}

/// GPU(`opencuda-directx`のChaCha20カーネル)で`data`を暗号化/復号する
/// (ストリーム暗号なので同じ演算)。RFC 8439のAEAD構成に合わせ、
/// **counter=1から**開始する(counter=0のブロックはPoly1305一時鍵
/// 導出専用のため、実データには使わない)。
fn gpu_chacha20_encrypt(
    dev: &Arc<dyn GpuDevice>,
    kernel: &CompiledKernel,
    key: &[u8; 32],
    nonce_bytes: &[u8],
    data: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut padded = data.to_vec();
    while padded.len() % 4 != 0 {
        padded.push(0);
    }

    let buf = opencuda_core::alloc_buffer(dev, padded.len())?;
    buf.copy_from_host(&padded)?;

    let key_words = to_u32_array_le::<8>(key)?;
    let nonce_words = to_u32_array_le::<3>(nonce_bytes)?;

    let mut args = vec![KernelArg::Ptr(buf.as_ptr())];
    for k in key_words.iter() {
        args.push(KernelArg::U32(*k));
    }
    for n in nonce_words.iter() {
        args.push(KernelArg::U32(*n));
    }
    args.push(KernelArg::U32(1)); // counter_base = 1 (RFC 8439、block 0はPoly1305鍵専用)

    let cfg = LaunchConfig::linear(1, 1);
    dev.launch_kernel(kernel, &cfg, &args).map_err(|e| anyhow::anyhow!("GPU kernel launch failed: {}", e))?;

    let mut result = vec![0u8; padded.len()];
    buf.copy_to_host(&mut result)?;
    result.truncate(data.len());
    Ok(result)
}

/// RFC 8439のPoly1305一時鍵をCPU(RustCrypto `chacha20`クレート)で
/// 導出する(counter=0のブロックの先頭32バイト)。この鍵導出自体は
/// 64バイトのみの計算でGPUオフロードの意味が無いため常にCPUで行う。
fn derive_poly1305_key(key: &[u8; 32], nonce_bytes: &[u8]) -> [u8; 32] {
    use chacha20::cipher::{KeyIvInit, StreamCipher};
    use chacha20::ChaCha20;

    let mut block = [0u8; 64];
    let mut cipher = ChaCha20::new(key.into(), nonce_bytes.into());
    cipher.apply_keystream(&mut block);
    let mut poly_key = [0u8; 32];
    poly_key.copy_from_slice(&block[..32]);
    poly_key
}

/// `pad16(x) = x`を16バイト境界まで0埋めしたもの(RFC 8439 §2.8)。
fn pad16(data: &mut Vec<u8>) {
    let rem = data.len() % 16;
    if rem != 0 {
        data.extend(std::iter::repeat(0u8).take(16 - rem));
    }
}

/// RFC 8439のPoly1305認証タグを計算する(AADは常に空)。
/// `ciphertext || pad16(ciphertext) || len(aad)=0 as u64 LE || len(ciphertext) as u64 LE`
/// をMACする。この構成が`chacha20poly1305`クレートの出力と完全一致
/// することをテストで検証済み(下記テスト参照)。
fn poly1305_tag(key: &[u8; 32], nonce_bytes: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    use poly1305::{universal_hash::KeyInit as Poly1305KeyInit, Poly1305};

    let poly_key = derive_poly1305_key(key, nonce_bytes);
    let mut mac_data = ciphertext.to_vec();
    pad16(&mut mac_data);
    mac_data.extend_from_slice(&0u64.to_le_bytes()); // AAD長 = 0
    mac_data.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());

    let mac = Poly1305::new((&poly_key).into());
    let tag = mac.compute_unpadded(&mac_data);
    let mut out = [0u8; 16];
    out.copy_from_slice(tag.as_slice());
    out
}

// ===== テスト =====
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
        // GPU が使えなければ CPU フォールバックするはず
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

    #[test]
    #[cfg(feature = "gpu")]
    fn gpu_backend_round_trip_if_available() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Gpu, &key);
        if accel.backend() == AccelBackend::Gpu {
            let plaintext = b"GPU accelerated tunnel data ".repeat(100);
            let sealed = accel.seal(&plaintext).unwrap();
            assert!(sealed.len() < plaintext.len());
            assert_eq!(accel.open(&sealed).unwrap(), plaintext);
        } else {
            assert_eq!(accel.backend(), AccelBackend::Cpu);
            let plaintext = b"fallback works";
            let sealed = accel.seal(plaintext).unwrap();
            assert_eq!(accel.open(&sealed).unwrap(), plaintext);
        }
    }

    /// 実GPUハードウェアが無い環境でもGPUバックエンドと同じ「Poly1305
    /// 一時鍵導出(counter=0)+実データ暗号化(counter=1)+MAC構成」を
    /// CPU実装で再現し、信頼できる`chacha20poly1305`クレートの出力と
    /// 完全一致することを検証する。`derive_poly1305_key`/`poly1305_tag`
    /// のAEAD構成ロジック自体が正しいこと(GPU実行の成否とは独立)を
    /// 保証するテスト。
    #[test]
    fn gpu_poly1305_construction_matches_chacha20poly1305_reference() {
        use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
        use chacha20::ChaCha20;
        use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, Key, KeyInit, Nonce};

        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let nonce_bytes: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let plaintext = b"tunnel payload for GPU AEAD construction cross-check, spanning multiple 64-byte blocks!!";

        // 信頼できるCPU実装(chacha20poly1305)の出力
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let expected = cipher.encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_slice()).unwrap();
        let (expected_ciphertext, expected_tag) = expected.split_at(expected.len() - 16);

        // GPUバックエンドと同じ構成をCPUで再現(counter=1から暗号化)
        let mut ct = plaintext.to_vec();
        let mut cipher2 = ChaCha20::new(&key.into(), &nonce_bytes.into());
        cipher2.seek(64u32); // 1ブロック(64バイト)分シークしてcounter=1相当にする
        cipher2.apply_keystream(&mut ct);
        let tag = poly1305_tag(&key, &nonce_bytes, &ct);

        assert_eq!(ct, expected_ciphertext, "ciphertext construction does not match chacha20poly1305 reference");
        assert_eq!(&tag, expected_tag, "poly1305_tag does not match chacha20poly1305 reference tag");
    }

    #[test]
    #[cfg(feature = "gpu")]
    fn gpu_backend_tampered_frame_is_rejected_if_available() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Gpu, &key);
        if accel.backend() != AccelBackend::Gpu {
            eprintln!("skipping: no GPU backend available in this environment");
            return;
        }
        let mut sealed = accel.seal(b"secret GPU tunnel data").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(accel.open(&sealed).is_err(), "GPU backend must reject a tampered frame");
    }
}