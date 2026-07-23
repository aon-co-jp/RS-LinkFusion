//! 特定サービス(動画/音楽ストリーミング等)向けの帯域制御(QoS、
//! オプトイン機能)。
//!
//! HiEndオーディオ用途で、ユーザーが指定したストリーミング系サービス
//! (動画配信・VOD・音楽配信等)向けのトラフィックだけ帯域上限(既定
//! 10Mbps)をかけ、それ以外のダウンロード/アップロードはボンディング
//! 接続の実効速度まで無制限に流す、という2層構成を選択制(既定オフ)
//! で提供する。
//!
//! **効果についての正直な開示**: 本モジュールが行うのは「指定した
//! 宛先向けの帯域に上限をかける」という中立的なトラフィック制御のみ。
//! 帯域を絞ることが音質(ジッター等)に与える効果自体について、本
//! プロジェクトは主張・保証しない——効果の有無はユーザーの環境・
//! 機材に依存する。
//!
//! ## 分類方式(正直な開示)
//! TUNはIPパケット層で動作するため、宛先ホスト名を直接は知らない。
//! そのため、トンネルを通過するDNSクエリ応答(UDP/53)を覗き見て
//! 「応答されたAレコードのホスト名→IPアドレス」の対応を一定時間
//! キャッシュし、設定済みのドメインサフィックス一覧と照合してIP
//! アドレス単位で分類する(市販のペアレンタルコントロールルーター
//! 等と同じ方式)。CDN・エニーキャストIPは複数サービスで共有される
//! ことがあるため、分類の精度は完全ではない(正直な限界)。CNAME
//! チェーンの深い解決は行わず、応答内のAレコードのみを見る
//! (大半の実運用DNS応答はA/CNAMEが同一応答に含まれるため、実用上は
//! 機能する設計)。IPv4のみ対応、IPv6は今回スコープ外。

use serde::Deserialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

const CLASSIFICATION_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Deserialize)]
pub struct QosConfig {
    /// このサフィックスに一致するホスト名への通信を帯域制限の対象とする
    pub streaming_suffixes: Vec<String>,
    /// ストリーミング判定されたトラフィックの帯域上限(Mbps)
    pub streaming_rate_mbps: f64,
}

impl QosConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// 主要な動画配信/VOD/音楽配信サービスをまとめた既定プリセット。
    /// ユーザー確認済みの例(Qobuz等)を含むが、完全な網羅は保証しない
    /// (`qos.toml`で自由に追加・変更できる)。
    pub fn default_streaming_preset() -> Self {
        Self {
            streaming_suffixes: vec![
                // 動画配信・VOD
                "netflix.com".into(),
                "nflxvideo.net".into(),
                "nflximg.net".into(),
                "unext.jp".into(),
                "youtube.com".into(),
                "googlevideo.com".into(),
                "ytimg.com".into(),
                "amazonvideo.com".into(),
                "primevideo.com".into(),
                "aiv-cdn.net".into(),
                "hulu.com".into(),
                "abema.tv".into(),
                "dmm.com".into(),
                "disneyplus.com".into(),
                "dssott.com".into(),
                // 音楽配信
                "spotify.com".into(),
                "scdn.co".into(),
                "qobuz.com".into(),
                "static.qobuz.com".into(),
                "music.apple.com".into(),
                "mzstatic.com".into(),
                "tidal.com".into(),
                "amazonmusic.com".into(),
            ],
            streaming_rate_mbps: 10.0,
        }
    }
}

/// DNS応答スヌーピングによるIPアドレス分類器。
pub struct Classifier {
    suffixes: Vec<String>,
    classified: StdMutex<HashMap<Ipv4Addr, Instant>>,
}

impl Classifier {
    pub fn new(suffixes: Vec<String>) -> Self {
        Self { suffixes: suffixes.into_iter().map(|s| s.to_lowercase()).collect(), classified: StdMutex::new(HashMap::new()) }
    }

    fn matches_suffix(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_lowercase();
        self.suffixes.iter().any(|suf| name == *suf || name.ends_with(&format!(".{suf}")))
    }

    /// IPv4パケット全体を渡す。UDP/53以外は即座に無視する(軽量なガード)。
    pub fn observe_ipv4_packet(&self, packet: &[u8]) {
        let Some((_src, _dst, proto, payload)) = parse_ipv4(packet) else { return };
        if proto != 17 {
            return;
        }
        let Some((src_port, dst_port, udp_payload)) = parse_udp(payload) else { return };
        if src_port != 53 && dst_port != 53 {
            return;
        }
        let Ok(dns) = simple_dns::Packet::parse(udp_payload) else { return };
        for answer in &dns.answers {
            if !self.matches_suffix(&answer.name.to_string()) {
                continue;
            }
            if let simple_dns::rdata::RData::A(a) = &answer.rdata {
                let ip = Ipv4Addr::from(a.address);
                self.classified.lock().unwrap().insert(ip, Instant::now() + CLASSIFICATION_TTL);
            }
        }
    }

    pub fn is_classified(&self, ip: Ipv4Addr) -> bool {
        let mut map = self.classified.lock().unwrap();
        if let Some(expiry) = map.get(&ip) {
            if *expiry >= Instant::now() {
                return true;
            }
            map.remove(&ip);
        }
        false
    }
}

fn parse_ipv4(packet: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr, u8, &[u8])> {
    if packet.len() < 20 || packet[0] >> 4 != 4 {
        return None;
    }
    let ihl = (packet[0] & 0x0F) as usize * 4;
    if packet.len() < ihl {
        return None;
    }
    let proto = packet[9];
    let src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
    let dst = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    Some((src, dst, proto, &packet[ihl..]))
}

fn parse_udp(payload: &[u8]) -> Option<(u16, u16, &[u8])> {
    if payload.len() < 8 {
        return None;
    }
    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    Some((src_port, dst_port, &payload[8..]))
}

/// パケットの宛先IPv4アドレス(送信方向の分類判定に使う)。
pub fn packet_dest_ipv4(packet: &[u8]) -> Option<Ipv4Addr> {
    parse_ipv4(packet).map(|(_, dst, _, _)| dst)
}

/// パケットの送信元IPv4アドレス(受信方向の分類判定に使う)。
pub fn packet_src_ipv4(packet: &[u8]) -> Option<Ipv4Addr> {
    parse_ipv4(packet).map(|(src, _, _, _)| src)
}

/// トークンバケット方式の帯域制限器。
pub struct RateLimiter {
    rate_bytes_per_sec: f64,
    burst_bytes: f64,
    state: AsyncMutex<RateLimiterState>,
}

struct RateLimiterState {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(mbps: f64) -> Self {
        let rate_bytes_per_sec = mbps * 1_000_000.0 / 8.0;
        let burst_bytes = rate_bytes_per_sec * 0.5;
        Self {
            rate_bytes_per_sec,
            burst_bytes,
            // 満タン状態から開始する(標準的なトークンバケットの初期化。
            // 起動直後の短いバーストを許容してから定常レートへ収束する)。
            state: AsyncMutex::new(RateLimiterState { tokens: burst_bytes, last: Instant::now() }),
        }
    }

    /// `bytes`分のトークンが貯まるまで待ってから消費する。`bytes`が
    /// バースト容量を超える場合は、容量まで貯まった時点で通過させ
    /// トークンをマイナスへ持ち越す(単発の巨大書き込みで永久に
    /// 待ち続けないようにするため——バケット容量固定のトークン
    /// バケットでは`bytes > capacity`のリクエストは本来満たせない)。
    pub async fn consume(&self, bytes: usize) {
        let target = (bytes as f64).min(self.burst_bytes);
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(state.last).as_secs_f64();
                state.last = now;
                state.tokens = (state.tokens + elapsed * self.rate_bytes_per_sec).min(self.burst_bytes);
                if state.tokens >= target {
                    state.tokens -= bytes as f64;
                    None
                } else {
                    let deficit = target - state.tokens;
                    Some(Duration::from_secs_f64(deficit / self.rate_bytes_per_sec))
                }
            };
            match wait {
                None => return,
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }
}

/// 分類器+帯域制限器のセット。`tun_gateway::relay_packets`から使う。
pub struct Qos {
    pub classifier: Classifier,
    pub limiter: RateLimiter,
}

impl Qos {
    pub fn new(config: QosConfig) -> Self {
        Self { classifier: Classifier::new(config.streaming_suffixes), limiter: RateLimiter::new(config.streaming_rate_mbps) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_matching_is_exact_or_subdomain_only() {
        let c = Classifier::new(vec!["netflix.com".to_string()]);
        assert!(c.matches_suffix("netflix.com"));
        assert!(c.matches_suffix("www.netflix.com"));
        assert!(c.matches_suffix("ipv4-c001-nrt001.1.oca.nflxvideo.net.netflix.com"));
        assert!(!c.matches_suffix("notnetflix.com"));
        assert!(!c.matches_suffix("example.com"));
    }

    #[tokio::test]
    async fn rate_limiter_delays_after_burst_is_exhausted() {
        let limiter = RateLimiter::new(8.0); // 8 Mbps = 1,000,000 bytes/sec, burst 500,000 bytes
        limiter.consume(500_000).await; // 満タンの初期バーストを即座に使い切る
        let started = Instant::now();
        limiter.consume(500_000).await; // バケットが空なので再充填を待つ必要がある
        assert!(started.elapsed() >= Duration::from_millis(400));
    }

    #[tokio::test]
    async fn rate_limiter_allows_initial_burst_without_delay() {
        let limiter = RateLimiter::new(8.0);
        let started = Instant::now();
        limiter.consume(400_000).await; // burst容量(500,000 bytes)以内は即座に通す
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn rate_limiter_does_not_hang_on_requests_larger_than_burst_capacity() {
        let limiter = RateLimiter::new(8.0); // burst容量は500,000 bytes
        tokio::time::timeout(Duration::from_secs(5), limiter.consume(2_000_000))
            .await
            .expect("consume() must not hang forever for a single chunk larger than burst capacity");
    }

    #[test]
    fn classified_entries_expire() {
        let c = Classifier::new(vec!["example.com".to_string()]);
        let ip = Ipv4Addr::new(93, 184, 216, 34);
        c.classified.lock().unwrap().insert(ip, Instant::now() - Duration::from_secs(1));
        assert!(!c.is_classified(ip));
    }
}
