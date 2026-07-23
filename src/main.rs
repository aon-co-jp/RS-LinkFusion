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
//! - `gateway-serve`/`gateway-connect`: TUN仮想アダプタ方式のフルVPN
//!   ゲートウェイ(`tun_gateway`モジュール参照)。固定1アドレスへの
//!   ポート転送ではなく、PC上のあらゆる通信をボンディング接続経由で
//!   流したい場合に使う。

mod accel;
mod framed;
#[cfg(feature = "gui")]
mod gui;
mod qos;
mod quality;
mod speedtest;
mod tun_gateway;

use accel::{AccelBackend, PayloadAccelerator};
use aggligator_transport_tcp::simple as agg_tcp;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quality::QualityTracker;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn default_history_path() -> PathBuf {
    PathBuf::from("speedtest-history.jsonl")
}

/// `qos_config`引数から`Qos`を組み立てる。`None`なら`None`(帯域制御
/// オフ)、`"default"`なら内蔵プリセット、それ以外はTOMLファイルパス
/// として読み込む。
fn load_qos(qos_config: Option<&str>) -> Result<Option<Arc<qos::Qos>>> {
    match qos_config {
        None => Ok(None),
        Some("default") => {
            tracing::info!("QoS: 内蔵プリセット(主要な動画/音楽配信サービスを10Mbpsへ制限)を使用");
            Ok(Some(Arc::new(qos::Qos::new(qos::QosConfig::default_streaming_preset()))))
        }
        Some(path) => {
            let config = qos::QosConfig::load(std::path::Path::new(path)).context("QoS設定ファイルの読み込みに失敗しました")?;
            tracing::info!(streaming_rate_mbps = config.streaming_rate_mbps, suffix_count = config.streaming_suffixes.len(), "QoS設定を読み込みました");
            Ok(Some(Arc::new(qos::Qos::new(config))))
        }
    }
}

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
    ///
    /// 各引数は環境変数でも指定できる(`RS_LINKFUSION_*`)。systemd等の
    /// サービスマネージャからは`ExecStart`に引数を書かず、
    /// `Environment=`だけで設定できるようにするため。
    Serve {
        /// ボンディング接続の受け付けアドレス(例: 0.0.0.0:5900)
        #[arg(long, env = "RS_LINKFUSION_BIND")]
        bind: SocketAddr,
        /// 転送先のローカルサービスアドレス(例: 127.0.0.1:8080)
        #[arg(long, env = "RS_LINKFUSION_TARGET")]
        target: SocketAddr,
        /// `generate-key`で生成したhex鍵
        #[arg(long, env = "RS_LINKFUSION_KEY")]
        key: String,
    },
    /// ローカル側: ローカルポートで待ち受け、ボンディング接続へ転送する。
    ///
    /// 各引数は環境変数でも指定できる(`RS_LINKFUSION_*`)。
    Connect {
        /// ローカル待ち受けアドレス(例: 127.0.0.1:8080)
        #[arg(long, env = "RS_LINKFUSION_LISTEN")]
        listen: SocketAddr,
        /// 接続先ホスト名/IPアドレス(カンマ区切りで複数指定可、`serve`側の`--bind`のIP/ホスト名)
        #[arg(long, env = "RS_LINKFUSION_REMOTE", value_delimiter = ',')]
        remote: Vec<String>,
        /// 接続先ポート(`serve`側の`--bind`のポート)
        #[arg(long, env = "RS_LINKFUSION_REMOTE_PORT")]
        remote_port: u16,
        /// `generate-key`で生成したhex鍵(`serve`側と同じ値)
        #[arg(long, env = "RS_LINKFUSION_KEY")]
        key: String,
    },
    /// TUNゲートウェイ・リモート側(典型的にはLinux VPS)。IPフォワーディング/
    /// NAT(MASQUERADE)の有効化は自動で行わない(README.md参照、手動設定が必要)。
    GatewayServe {
        /// ボンディング接続の受け付けアドレス(例: 0.0.0.0:5900)
        #[arg(long, env = "RS_LINKFUSION_BIND")]
        bind: SocketAddr,
        /// TUNインターフェースに割り当てるIPv4アドレス
        #[arg(long, env = "RS_LINKFUSION_TUN_ADDR", default_value = "10.66.0.1")]
        tun_addr: Ipv4Addr,
        /// TUNインターフェースのプレフィクス長
        #[arg(long, env = "RS_LINKFUSION_TUN_PREFIX", default_value_t = 24)]
        tun_prefix: u8,
        /// TUNインターフェースのMTU
        #[arg(long, env = "RS_LINKFUSION_MTU", default_value_t = 1400)]
        mtu: u16,
        /// `generate-key`で生成したhex鍵
        #[arg(long, env = "RS_LINKFUSION_KEY")]
        key: String,
        /// QoS設定TOMLファイル(streaming_suffixes/streaming_rate_mbps)。
        /// 未指定なら帯域制御は行わない(既定オフ)。`default`を渡すと
        /// 主要な動画/音楽配信サービスの内蔵プリセットを使う。
        #[arg(long, env = "RS_LINKFUSION_QOS_CONFIG")]
        qos_config: Option<String>,
    },
    /// TUNゲートウェイ・ローカル側(Windows等)。管理者権限、Windowsでは
    /// `wintun.dll`が実行ファイルと同じディレクトリに必要(README.md参照)。
    /// デフォルトルートのTUN経由への切り替えは自動で行わない。
    GatewayConnect {
        /// 接続先ホスト名/IPアドレス(カンマ区切りで複数指定可)
        #[arg(long, env = "RS_LINKFUSION_REMOTE", value_delimiter = ',')]
        remote: Vec<String>,
        /// 接続先ポート(`gateway-serve`側の`--bind`のポート)
        #[arg(long, env = "RS_LINKFUSION_REMOTE_PORT")]
        remote_port: u16,
        /// TUNインターフェースに割り当てるIPv4アドレス
        #[arg(long, env = "RS_LINKFUSION_TUN_ADDR", default_value = "10.66.0.2")]
        tun_addr: Ipv4Addr,
        /// TUNインターフェースのプレフィクス長
        #[arg(long, env = "RS_LINKFUSION_TUN_PREFIX", default_value_t = 24)]
        tun_prefix: u8,
        /// TUNインターフェースのMTU(`gateway-serve`側と一致させること)
        #[arg(long, env = "RS_LINKFUSION_MTU", default_value_t = 1400)]
        mtu: u16,
        /// `generate-key`で生成したhex鍵(`gateway-serve`側と同じ値)
        #[arg(long, env = "RS_LINKFUSION_KEY")]
        key: String,
        /// QoS設定TOMLファイル。未指定なら帯域制御は行わない(既定オフ)。
        /// `default`で主要な動画/音楽配信サービスの内蔵プリセットを使う。
        #[arg(long, env = "RS_LINKFUSION_QOS_CONFIG")]
        qos_config: Option<String>,
    },
    /// ネット速度測定(M-Lab/ndt7)・自動記録・履歴管理。
    SpeedTest {
        #[command(subcommand)]
        command: SpeedTestCommand,
    },
    /// GUIウィンドウを起動する(「速度測定」ボタン等)。`gui` feature必須。
    Gui,
}

#[derive(Subcommand)]
enum SpeedTestCommand {
    /// M-Lab(ndt7)で1回速度測定する
    Run {
        /// 記録に付けるラベル(例: baseline, accelerated)
        #[arg(long, default_value = "manual")]
        label: String,
        /// 対話的な同意確認をスキップする
        #[arg(long)]
        yes: bool,
        /// 履歴ファイルのパス
        #[arg(long, default_value = "speedtest-history.jsonl")]
        history: PathBuf,
    },
    /// M-Lab(ndt7)を一定間隔で自動測定・自動記録し続ける(確認なし、Ctrl+Cで終了)
    Watch {
        /// 測定間隔(分)
        #[arg(long, default_value_t = 60)]
        interval_minutes: u64,
        #[arg(long, default_value = "auto")]
        label: String,
        #[arg(long, default_value = "speedtest-history.jsonl")]
        history: PathBuf,
    },
    /// gate02/osakagas等、非公式サイトを手動で開いて読み取った値を記録する
    RecordManual {
        /// 測定元(例: gate02, osakagas)
        #[arg(long)]
        source: String,
        #[arg(long, default_value = "manual")]
        label: String,
        #[arg(long)]
        download_mbps: f64,
        #[arg(long)]
        upload_mbps: f64,
        #[arg(long, default_value = "speedtest-history.jsonl")]
        history: PathBuf,
    },
    /// gate02/osakagas等、手動確認用サイトのURL一覧を表示する
    Links,
    /// 記録済みの測定履歴を表示する
    History {
        #[arg(long, default_value = "speedtest-history.jsonl")]
        history: PathBuf,
    },
    /// 古くなった記録を確認のうえまとめて削除する
    Prune {
        /// これより古い記録を削除対象にする(日数)
        #[arg(long, default_value_t = 90)]
        older_than_days: i64,
        /// 確認をスキップする
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "speedtest-history.jsonl")]
        history: PathBuf,
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
        Command::GatewayServe { bind, tun_addr, tun_prefix, mtu, key, qos_config } => {
            let key = decode_hex_key(&key)?;
            let qos = load_qos(qos_config.as_deref())?;
            run_gateway_serve(bind, tun_addr, tun_prefix, mtu, key, qos).await?;
        }
        Command::GatewayConnect { remote, remote_port, tun_addr, tun_prefix, mtu, key, qos_config } => {
            let key = decode_hex_key(&key)?;
            let qos = load_qos(qos_config.as_deref())?;
            run_gateway_connect(remote, remote_port, tun_addr, tun_prefix, mtu, key, qos).await?;
        }
        Command::SpeedTest { command } => run_speedtest_command(command).await?,
        Command::Gui => {
            #[cfg(feature = "gui")]
            {
                gui::run(default_history_path())?;
            }
            #[cfg(not(feature = "gui"))]
            {
                anyhow::bail!("この実行ファイルは`gui` featureを無効にしてビルドされているため、GUIは使えません");
            }
        }
    }

    Ok(())
}

async fn run_speedtest_command(command: SpeedTestCommand) -> Result<()> {
    match command {
        SpeedTestCommand::Run { label, yes, history } => {
            let record = speedtest::run(label, &history, yes).await?;
            println!(
                "download: {:.1} Mbps / upload: {:.1} Mbps / min RTT: {}",
                record.download_mbps,
                record.upload_mbps,
                record.min_rtt_ms.map(|v| format!("{v:.1} ms")).unwrap_or_else(|| "N/A".to_string())
            );
        }
        SpeedTestCommand::Watch { interval_minutes, label, history } => {
            speedtest::watch(label, &history, std::time::Duration::from_secs(interval_minutes * 60), false).await?;
        }
        SpeedTestCommand::RecordManual { source, label, download_mbps, upload_mbps, history } => {
            speedtest::record_manual(source, label, download_mbps, upload_mbps, &history)?;
            println!("記録しました。");
        }
        SpeedTestCommand::Links => {
            println!("M-Lab(自動測定対応): `rs-linkfusion speedtest run` で実行できます。");
            for (name, url) in speedtest::MANUAL_REFERENCE_SITES {
                println!("{name}(手動確認用): {url}");
            }
        }
        SpeedTestCommand::History { history } => {
            for record in speedtest::load_history(&history)? {
                println!(
                    "[{}] {} / {}: down {:.1} Mbps, up {:.1} Mbps (interfaces: {})",
                    record.recorded_at, record.source, record.label, record.download_mbps, record.upload_mbps, record.environment.interface_count
                );
            }
        }
        SpeedTestCommand::Prune { older_than_days, yes, history } => {
            speedtest::prune_older_than(&history, older_than_days, yes)?;
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

/// TUNゲートウェイ・リモート側。1接続のみを受け付け、TUNデバイスと
/// ボンディング接続の間でIPパケットを中継する(複数クライアント同時
/// 接続時のパケット混線防止は未実装、単一クライアント前提の設計)。
async fn run_gateway_serve(
    bind: SocketAddr,
    tun_addr: Ipv4Addr,
    tun_prefix: u8,
    mtu: u16,
    key: [u8; 32],
    qos: Option<Arc<qos::Qos>>,
) -> Result<()> {
    let accel = Arc::new(PayloadAccelerator::new(AccelBackend::Cpu, &key));
    let tun = tun_gateway::create_tun_device(tun_addr, tun_prefix, mtu)?;
    tracing::info!(%bind, %tun_addr, tun_prefix, mtu, backend = ?accel.backend(), qos_enabled = qos.is_some(), "starting TUN gateway server");

    agg_tcp::tcp_server(bind, move |stream| {
        let accel = Arc::clone(&accel);
        let tun = Arc::clone(&tun);
        let quality = Arc::new(QualityTracker::new());
        let qos = qos.clone();
        async move {
            if let Err(e) = tun_gateway::relay_packets(tun, stream, accel, mtu as usize, quality, qos).await {
                tracing::warn!(error = %e, "gateway-serve relay ended with error");
            }
        }
    })
    .await
    .context("bonded tcp_server failed")?;

    Ok(())
}

/// TUNゲートウェイ・ローカル側。TUNデバイスを作成し、`remote`へ
/// ボンディング接続を張ってIPパケットを中継する。
///
/// **自動再接続(ユーザー指示、2026-07-23)**: WAN/LAN/WiFiの構成が
/// システム側で変化しても(回線切断・新規接続・全リンク一時喪失等)、
/// このループが無人で再接続を試み続ける。個々の物理インターフェース
/// の追加/削除自体は`aggligator`(`TcpConnector::link_tags`)が内部で
/// 10秒間隔(デフォルト)で自動再走査しているため、リンクが1本でも
/// 生きていればこのループの外側で自動的に吸収される——ここで扱うのは
/// 「ボンディング接続が完全に切断された(全リンク喪失)場合の、
/// 接続そのものの再確立」。再試行間隔はRS-SmartTCPの
/// `AdaptivePolicy`(実測RTT/ジッターに基づくFast/Slow判定)に従って
/// 自動調整される(光回線級なら短い間隔、通常回線なら保守的な間隔)。
async fn run_gateway_connect(
    remote: Vec<String>,
    remote_port: u16,
    tun_addr: Ipv4Addr,
    tun_prefix: u8,
    mtu: u16,
    key: [u8; 32],
    qos: Option<Arc<qos::Qos>>,
) -> Result<()> {
    let accel = Arc::new(PayloadAccelerator::new(AccelBackend::Cpu, &key));
    let tun = tun_gateway::create_tun_device(tun_addr, tun_prefix, mtu)?;
    let quality = Arc::new(QualityTracker::new());
    tracing::info!(?remote, remote_port, %tun_addr, tun_prefix, mtu, backend = ?accel.backend(), qos_enabled = qos.is_some(), "starting TUN gateway client (auto-reconnect enabled)");

    loop {
        tracing::info!(?remote, remote_port, "establishing bonded connection to gateway-serve");
        match agg_tcp::tcp_connect(remote.clone(), remote_port).await {
            Ok(agg_stream) => {
                tracing::info!("bonded connection established, relaying packets");
                if let Err(e) = tun_gateway::relay_packets(
                    Arc::clone(&tun),
                    agg_stream,
                    Arc::clone(&accel),
                    mtu as usize,
                    Arc::clone(&quality),
                    qos.clone(),
                )
                .await
                {
                    tracing::warn!(error = %e, "gateway-connect relay ended, will attempt to reconnect");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to establish bonded connection, will retry");
            }
        }

        quality.log_status();
        let backoff = quality.policy().retry_backoff();
        tracing::info!(?backoff, mode = ?quality.policy().mode(), "waiting before reconnect attempt (RS-SmartTCP adaptive backoff)");
        tokio::time::sleep(backoff).await;
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
