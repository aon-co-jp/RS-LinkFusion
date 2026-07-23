//! `rs-linkfusion` CLIエントリポイント。
//!
//! 複数WAN/LAN/WiFiインターフェース(`aggligator`が自動列挙する、OSが
//! 認識するすべてのネットワークインターフェース)を1つの論理接続へ
//! 束ね、その上に`framed`モジュールの圧縮+暗号化フレームで任意のTCP
//! トラフィックを流す「ミニVPN的トンネル」を提供する。
//!
//! - `serve`: リモート側。ボンディング接続を受け付け、ローカルの
//!   実サービス(例: `127.0.0.1:8080`)へリバースプロキシする。
//! - `connect`: ローカル側。ローカルポートで待ち受け、接続ごとに
//!   `serve`側へのボンディング接続を新規に張って転送する。

mod accel;
mod framed;
mod quality;

use accel::{AccelBackend, PayloadAccelerator};
use aggligator_transport_tcp::simple as agg_tcp;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quality::QualityTracker;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CHUNK_SIZE: usize = 16 * 1024;

#[derive(Parser)]
#[command(
    name = "rs-linkfusion",
    version,
    about = "複数WAN/LAN/WiFiインターフェースをaggligatorで束ね(ボンディング)、通信の高速化・安定化を実現するトンネル"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 新しい暗号鍵(32バイト、hex表示)を生成する。serve/connect双方に同じ鍵を渡すこと。
    GenerateKey,
    /// リモート側: ボンディング接続を受け付け、ローカルサービスへリバースプロキシする。
    Serve {
        /// ボンディング接続の受け付けアドレス(例: 0.0.0.0:5900)
        #[arg(long)]
        bind: SocketAddr,
        /// 転送先のローカルサービスアドレス(例: 127.0.0.1:8080)
        #[arg(long)]
        target: SocketAddr,
        /// `generate-key`で生成したhex鍵
        #[arg(long)]
        key: String,
    },
    /// ローカル側: ローカルポートで待ち受け、ボンディング接続へ転送する。
    Connect {
        /// ローカル待ち受けアドレス(例: 127.0.0.1:8080)
        #[arg(long)]
        listen: SocketAddr,
        /// 接続先ホスト名/IPアドレス(カンマ区切りで複数指定可、`serve`側の`--bind`のIP/ホスト名)
        #[arg(long, value_delimiter = ',')]
        remote: Vec<String>,
        /// 接続先ポート(`serve`側の`--bind`のポート)
        #[arg(long)]
        remote_port: u16,
        /// `generate-key`で生成したhex鍵(`serve`側と同じ値)
        #[arg(long)]
        key: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::GenerateKey => {
            let key = PayloadAccelerator::generate_key();
            println!("{}", encode_hex(&key));
        }
        Command::Serve { bind, target, key } => {
            let key = decode_hex_key(&key)?;
            run_serve(bind, target, key).await?;
        }
        Command::Connect { listen, remote, remote_port, key } => {
            let key = decode_hex_key(&key)?;
            run_connect(listen, remote, remote_port, key).await?;
        }
    }

    Ok(())
}

async fn run_serve(bind: SocketAddr, target: SocketAddr, key: [u8; 32]) -> Result<()> {
    let accel = Arc::new(PayloadAccelerator::new(AccelBackend::Cpu, &key));
    tracing::info!(%bind, %target, backend = ?accel.backend(), "starting bonded tunnel server");

    agg_tcp::tcp_server(bind, move |stream| {
        let accel = Arc::clone(&accel);
        async move {
            if let Err(e) = handle_serve_connection(stream, target, accel).await {
                tracing::warn!(error = %e, "serve connection ended with error");
            }
        }
    })
    .await
    .context("bonded tcp_server failed")?;

    Ok(())
}

async fn handle_serve_connection(
    agg_stream: aggligator::alc::Stream,
    target: SocketAddr,
    accel: Arc<PayloadAccelerator>,
) -> Result<()> {
    let local = TcpStream::connect(target).await.context("connecting to local target service")?;
    relay(agg_stream, local, accel).await
}

async fn run_connect(listen: SocketAddr, remote_hosts: Vec<String>, remote_port: u16, key: [u8; 32]) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await.context("binding local listen address")?;
    let accel = Arc::new(PayloadAccelerator::new(AccelBackend::Cpu, &key));
    tracing::info!(%listen, ?remote_hosts, remote_port, backend = ?accel.backend(), "starting bonded tunnel client");

    loop {
        let (local, peer) = listener.accept().await?;
        let accel = Arc::clone(&accel);
        let hosts = remote_hosts.clone();

        tokio::spawn(async move {
            match agg_tcp::tcp_connect(hosts, remote_port).await {
                Ok(agg_stream) => {
                    if let Err(e) = relay(agg_stream, local, accel).await {
                        tracing::warn!(error = %e, %peer, "connect relay ended with error");
                    }
                }
                Err(e) => tracing::warn!(error = %e, %peer, "failed to establish bonded connection"),
            }
        });
    }
}

/// ボンディング接続(圧縮+暗号化フレーム)とローカルTCP接続(平文)の間で
/// 双方向にデータを中継する。
async fn relay(agg_stream: aggligator::alc::Stream, local: TcpStream, accel: Arc<PayloadAccelerator>) -> Result<()> {
    let (mut local_rd, mut local_wr) = local.into_split();
    let (mut agg_rd, mut agg_wr) = tokio::io::split(agg_stream);
    let quality = QualityTracker::new();

    let to_agg = async {
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            framed::write_frame(&mut agg_wr, &accel, &buf[..n]).await?;
        }
        anyhow::Ok(())
    };

    let to_local = async {
        loop {
            let started = Instant::now();
            match framed::read_frame(&mut agg_rd, &accel).await? {
                Some(data) => {
                    quality.record_round_trip(started);
                    local_wr.write_all(&data).await?;
                }
                None => break,
            }
        }
        anyhow::Ok(())
    };

    let result = tokio::try_join!(to_agg, to_local);
    quality.log_status();
    result?;
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex_key(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 {
        anyhow::bail!("key must be 64 hex characters (32 bytes), got {} characters", s.len());
    }
    let mut out = [0u8; 32];
    for (i, chunk) in out.iter_mut().enumerate() {
        let byte_str = &s[i * 2..i * 2 + 2];
        *chunk = u8::from_str_radix(byte_str, 16).context("key must be valid hex")?;
    }
    Ok(out)
}
