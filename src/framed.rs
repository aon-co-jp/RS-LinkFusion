//! 長さプレフィクス付きフレームで、圧縮+暗号化されたペイロードを
//! 非同期ストリーム上に送受信するヘルパー。

use crate::accel::PayloadAccelerator;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 1フレーム(圧縮+暗号化済み)を`[len:u32 LE][data]`形式で書き込む。
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, accel: &PayloadAccelerator, plaintext: &[u8]) -> anyhow::Result<()> {
    let sealed = accel.seal(plaintext)?;
    let len = (sealed.len() as u32).to_le_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&sealed).await?;
    writer.flush().await?;
    Ok(())
}

/// 1フレームを読み取り、復号+解凍した平文を返す。EOFなら`Ok(None)`。
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, accel: &PayloadAccelerator) -> anyhow::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut sealed = vec![0u8; len];
    reader.read_exact(&mut sealed).await?;
    Ok(Some(accel.open(&sealed)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accel::AccelBackend;

    #[tokio::test]
    async fn frame_round_trips_over_a_real_in_memory_duplex_stream() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Cpu, &key);
        let (mut a, mut b) = tokio::io::duplex(4096);

        write_frame(&mut a, &accel, b"hello over the bonded tunnel").await.unwrap();
        let received = read_frame(&mut b, &accel).await.unwrap().unwrap();
        assert_eq!(received, b"hello over the bonded tunnel");
    }

    #[tokio::test]
    async fn multiple_frames_in_sequence_are_read_back_in_order() {
        let key = PayloadAccelerator::generate_key();
        let accel = PayloadAccelerator::new(AccelBackend::Cpu, &key);
        let (mut a, mut b) = tokio::io::duplex(8192);

        for msg in [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()] {
            write_frame(&mut a, &accel, &msg).await.unwrap();
        }
        drop(a);

        let mut out = Vec::new();
        while let Some(frame) = read_frame(&mut b, &accel).await.unwrap() {
            out.push(frame);
        }
        assert_eq!(out, vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]);
    }
}
