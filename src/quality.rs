//! RS-SmartTCP(IOWN/APN×Smart-TCPの良いとこ取り適応制御)を使い、
//! ボンディング接続の実測RTTを追跡・ログ出力する。
//!
//! `aggligator::alc::Stream::links()`/`stats()`から得られるリンク単位の
//! 統計を直接使うのではなく、アプリ層での単純な往復計測(1フレームの
//! 送受信完了までの時間)を`RS-SmartTCP::NetworkQualityMonitor`へ記録
//! する、という単純化した設計(個々の物理リンクごとの内訳ではなく、
//! ボンディングされた論理接続全体の実効品質を見る)。

use rs_smarttcp::{AdaptivePolicy, NetworkQualityMonitor};
use std::sync::Arc;
use std::time::Instant;

pub struct QualityTracker {
    policy: Arc<AdaptivePolicy>,
}

impl QualityTracker {
    pub fn new() -> Self {
        Self { policy: Arc::new(AdaptivePolicy::new(NetworkQualityMonitor::new())) }
    }

    pub fn policy(&self) -> Arc<AdaptivePolicy> {
        Arc::clone(&self.policy)
    }

    /// 1回の往復(フレーム送信〜応答受信)にかかった時間を記録する。
    pub fn record_round_trip(&self, started: Instant) {
        self.policy.monitor().record_rtt(started.elapsed());
    }

    pub fn log_status(&self) {
        let mode = self.policy.mode();
        tracing::info!(
            ?mode,
            srtt_ms = ?self.policy.monitor().smoothed_rtt_ms(),
            rttvar_ms = ?self.policy.monitor().rttvar_ms(),
            "bonded tunnel quality"
        );
    }
}

impl Default for QualityTracker {
    fn default() -> Self {
        Self::new()
    }
}
