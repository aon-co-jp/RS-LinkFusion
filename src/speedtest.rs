//! ネット速度測定(「高速化前」/「高速化後」の比較記録用)。
//!
//! **自動化するのはM-Labのみ**(Google検索の速度テストウィジェットが
//! 実際に使っているのと同じ基盤、[M-Lab](https://www.measurementlab.net/)
//! の`ndt7`プロトコル、公開・研究目的のインターネット速度測定
//! インフラ)。`ndt7-client`クレート経由で直接叩く。
//!
//! **gate02・osakagasは自動化しない(正直な開示)**: この2サイトは
//! 人間がブラウザで使うことを前提としたJS製ウィジェットで、公開APIが
//! 無い。裏側の非公式エンドポイントを解析して自動アクセスすることは
//! 利用規約違反・仕様変更による破損のリスクがあるため行わない
//! (ユーザー確認済み、2026-07-23)。代わりに`speedtest links`で
//! URLを表示し、ユーザーが手動で開いて読み取った数値を
//! `speedtest record-manual`で同じ履歴ファイルへ記録できるようにする。
//!
//! **同意についての正直な開示**: `ndt7`テストを実行すると、M-Labの
//! Locate APIへ接続先探索リクエストが送られ、IPアドレスが共有される
//! (Googleのウィジェットの注記と同じ仕組み——テスト結果はインター
//! ネット研究促進のためM-Labにより公開される)。`speedtest run`は
//! `--yes`が無い限り対話的に同意確認を行う。`speedtest watch`
//! (定期自動測定)は起動時に一度だけ同意確認を行い、以後は無人で
//! 繰り返す。
//!
//! 測定のたびに、その時点のネットワーク環境(物理インターフェース数・
//! 有線/無線の内訳)も自動検出して記録する。「高速化後」の測定は、
//! `gateway-connect`でデフォルトルートをボンディング接続経由(TUN)に
//! 切り替えた状態でこのサブコマンドを実行することで、実際にボンディ
//! ング接続を通した速度を計測できる——このモジュール自身は「今の
//! OSのデフォルトルートで測る」だけであり、経路の切り替え自体は
//! ユーザー側の操作(README.md参照)に委ねる設計。

use anyhow::{Context, Result};
use ndt7_client::client::ClientBuilder;
use ndt7_client::spec::Origin;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{BufRead, Write as _};
use std::path::Path;

/// 手動で読み取った数値を記録できる参照用サイト。
pub const MANUAL_REFERENCE_SITES: &[(&str, &str)] = &[
    ("gate02", "https://speedtest.gate02.ne.jp/"),
    ("osakagas", "https://speedcheck.osakagas.co.jp/#!/?=true"),
];

/// 通信インターフェースの種別(名前に基づく推定、正確な種別判定は
/// OS別APIが必要なため今回はベストエフォートのヒューリスティック)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceKind {
    Ethernet,
    Wifi,
    Other,
}

fn classify_interface_name(name: &str) -> InterfaceKind {
    let lower = name.to_lowercase();
    if lower.contains("wi-fi")
        || lower.contains("wifi")
        || lower.contains("wlan")
        || lower.contains("wireless")
        || lower.starts_with("wlp")
        || lower.starts_with("wlx")
    {
        InterfaceKind::Wifi
    } else if lower.contains("ethernet") || lower.contains("eth") || lower.contains("lan") || lower.starts_with("en")
    {
        InterfaceKind::Ethernet
    } else {
        InterfaceKind::Other
    }
}

/// 測定時点のネットワーク環境(WAN側の複数回線構成を直接見ることは
/// できないため、あくまで「このマシンが持つ物理/仮想インター
/// フェースの本数・内訳」を記録する)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEnvironment {
    pub interface_count: usize,
    pub ethernet_count: usize,
    pub wifi_count: usize,
    pub other_count: usize,
    pub interface_names: Vec<String>,
}

pub fn detect_environment() -> Result<NetworkEnvironment> {
    use network_interface::NetworkInterfaceConfig;
    let interfaces =
        network_interface::NetworkInterface::show().context("failed to enumerate network interfaces")?;

    let mut names = Vec::new();
    let mut ethernet_count = 0;
    let mut wifi_count = 0;
    let mut other_count = 0;
    let mut seen = HashSet::new();

    for iface in &interfaces {
        let lower = iface.name.to_lowercase();
        if lower.contains("loopback") || lower == "lo" {
            continue;
        }
        if !seen.insert(iface.name.clone()) {
            continue;
        }
        match classify_interface_name(&iface.name) {
            InterfaceKind::Ethernet => ethernet_count += 1,
            InterfaceKind::Wifi => wifi_count += 1,
            InterfaceKind::Other => other_count += 1,
        }
        names.push(iface.name.clone());
    }

    Ok(NetworkEnvironment {
        interface_count: names.len(),
        ethernet_count,
        wifi_count,
        other_count,
        interface_names: names,
    })
}

/// 1回分の速度測定結果。`label`で「baseline(高速化前)」
/// 「accelerated(高速化後)」等を区別する。`source`で測定元
/// (`mlab`=自動測定、`gate02`/`osakagas`=手動記録)を区別する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedTestRecord {
    pub source: String,
    pub label: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
    pub environment: NetworkEnvironment,
    pub download_mbps: f64,
    pub upload_mbps: f64,
    pub min_rtt_ms: Option<f64>,
}

/// 標準入力で対話的に同意を確認する(`ndt7`はM-Labへ接続しIPが共有
/// されるため)。`skip_prompt`が`true`(`--yes`)なら確認をスキップする。
fn confirm_mlab_consent(skip_prompt: bool) -> Result<()> {
    if skip_prompt {
        return Ok(());
    }
    println!("このネット速度テストは M-Lab(Measurement Lab)の公開インフラ(ndt7プロトコル)へ接続します。");
    println!("実行すると、あなたのIPアドレスがM-Labへ共有され、テスト結果はインターネット研究促進のためM-Labにより公開されます。");
    print!("テストを実行しますか? [y/N]: ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).context("failed to read confirmation from stdin")?;
    if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
        anyhow::bail!("ユーザーが速度テストの実行に同意しなかったため中止しました");
    }
    Ok(())
}

/// 同意確認のうえ、`ndt7`(M-Lab)で1回速度測定を行い、結果を
/// `history_path`へ追記する。
pub async fn run(label: String, history_path: &Path, skip_prompt: bool) -> Result<SpeedTestRecord> {
    confirm_mlab_consent(skip_prompt)?;
    run_mlab_measurement(label, history_path).await
}

/// `interval`ごとに無人で繰り返し測定する(Ctrl+Cまたは呼び出し元の
/// キャンセルで停止)。同意確認は開始時に一度だけ行う。
pub async fn watch(label: String, history_path: &Path, interval: std::time::Duration, skip_prompt: bool) -> Result<()> {
    confirm_mlab_consent(skip_prompt)?;
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        match run_mlab_measurement(label.clone(), history_path).await {
            Ok(record) => {
                tracing::info!(download_mbps = record.download_mbps, upload_mbps = record.upload_mbps, "speedtest watch: 測定完了");
            }
            Err(e) => {
                tracing::warn!(error = %e, "speedtest watch: 測定失敗(次回間隔で再試行)");
            }
        }
    }
}

async fn run_mlab_measurement(label: String, history_path: &Path) -> Result<SpeedTestRecord> {
    let environment = detect_environment()?;

    let client = ClientBuilder::new("rs-linkfusion", env!("CARGO_PKG_VERSION")).build();
    let targets = client.locate_test_targets().await.context("M-Lab Locate APIから近隣サーバーを取得できませんでした")?;

    let mut download_mbps = 0.0;
    let mut min_rtt_ms = None;
    if let Some(url) = &targets.download_url {
        let mut rx = client.start_download(url).await.context("ndt7ダウンロードテストの開始に失敗しました")?;
        while let Some(result) = rx.recv().await {
            let m = result?;
            if m.origin == Some(Origin::Client) {
                if let Some(app) = &m.app_info {
                    if app.elapsed_time > 0 {
                        download_mbps = 8.0 * app.num_bytes as f64 / app.elapsed_time as f64;
                    }
                }
            }
            if m.origin == Some(Origin::Server) {
                if let Some(tcp) = &m.tcp_info {
                    if let Some(rtt) = tcp.min_rtt {
                        min_rtt_ms = Some(rtt as f64 / 1000.0);
                    }
                }
            }
        }
    }

    let mut upload_mbps = 0.0;
    if let Some(url) = &targets.upload_url {
        let mut rx = client.start_upload(url).await.context("ndt7アップロードテストの開始に失敗しました")?;
        while let Some(result) = rx.recv().await {
            let m = result?;
            if m.origin == Some(Origin::Server) {
                if let Some(tcp) = &m.tcp_info {
                    if let (Some(received), Some(elapsed)) = (tcp.bytes_received, tcp.elapsed_time) {
                        if elapsed > 0 {
                            upload_mbps = 8.0 * received as f64 / elapsed as f64;
                        }
                    }
                }
            }
        }
    }

    let record = SpeedTestRecord {
        source: "mlab".to_string(),
        label,
        recorded_at: chrono::Utc::now(),
        environment,
        download_mbps,
        upload_mbps,
        min_rtt_ms,
    };

    append_history(history_path, &record)?;
    Ok(record)
}

/// gate02/osakagas等、非公式サイトを手動で開いて読み取った値を
/// 同じ履歴ファイルへ記録する。
pub fn record_manual(
    source: String,
    label: String,
    download_mbps: f64,
    upload_mbps: f64,
    history_path: &Path,
) -> Result<SpeedTestRecord> {
    let record = SpeedTestRecord {
        source,
        label,
        recorded_at: chrono::Utc::now(),
        environment: detect_environment().unwrap_or(NetworkEnvironment {
            interface_count: 0,
            ethernet_count: 0,
            wifi_count: 0,
            other_count: 0,
            interface_names: Vec::new(),
        }),
        download_mbps,
        upload_mbps,
        min_rtt_ms: None,
    };
    append_history(history_path, &record)?;
    Ok(record)
}

fn append_history(path: &Path, record: &SpeedTestRecord) -> Result<()> {
    let line = serde_json::to_string(record)?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).context("opening speed test history file")?;
    writeln!(file, "{line}").context("writing speed test history record")?;
    Ok(())
}

/// 履歴ファイル(JSONL、1行1レコード)を全件読み込む。
pub fn load_history(path: &Path) -> Result<Vec<SpeedTestRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path).context("opening speed test history file")?;
    let reader = std::io::BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line).context("parsing speed test history record")?);
    }
    Ok(records)
}

/// `older_than_days`より古い記録を、確認(`skip_prompt`が`false`の場合)
/// のうえまとめて削除する。削除した件数を返す。
pub fn prune_older_than(path: &Path, older_than_days: i64, skip_prompt: bool) -> Result<usize> {
    let records = load_history(path)?;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days);
    let (keep, remove): (Vec<_>, Vec<_>) = records.into_iter().partition(|r| r.recorded_at >= cutoff);

    if remove.is_empty() {
        println!("{older_than_days}日より古い記録はありませんでした。");
        return Ok(0);
    }

    if !skip_prompt {
        let oldest = remove.iter().map(|r| r.recorded_at).min().unwrap();
        println!("{}件の記録が{older_than_days}日より古くなっています(最も古い記録: {oldest})。", remove.len());
        print!("まとめて削除してよろしいですか? [y/N]: ");
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).context("failed to read confirmation from stdin")?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("削除を中止しました。");
            return Ok(0);
        }
    }

    let tmp_path = path.with_extension("jsonl.tmp");
    {
        let mut file = std::fs::File::create(&tmp_path).context("creating temporary history file")?;
        for record in &keep {
            writeln!(file, "{}", serde_json::to_string(record)?)?;
        }
    }
    std::fs::rename(&tmp_path, path).context("replacing history file with pruned version")?;

    println!("{}件の古い記録を削除しました({}件を保持)。", remove.len(), keep.len());
    Ok(remove.len())
}
