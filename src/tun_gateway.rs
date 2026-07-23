//! TUN仮想アダプタ方式のフルVPNゲートウェイ。
//!
//! `main.rs`本体の`serve`/`connect`(固定1アドレスへのポート転送)とは
//! 別の、より野心的なモード。OSレベルでIPパケットを丸ごと捕捉する
//! `TUN`インターフェースを使い、PC上のあらゆる通信(ブラウザ・ゲーム・
//! Zoom等)をボンディング接続経由で流せるようにする。
//!
//! - `gateway-serve`: リモート側(典型的にはLinux VPS)。TUNインター
//!   フェースを持ち、クライアントから受け取ったIPパケットをTUNへ
//!   書き込む。**IPフォワーディング・NAT(MASQUERADE)の有効化は
//!   このプロセス自身では行わない**(後述「正直な開示」参照)——
//!   OSのファイアウォール/ルーティング設定を無断で書き換えるのは
//!   影響範囲が大きすぎるため、必要なコマンドをドキュメント
//!   (README.md/CLAUDE.md)に明記し、運用者自身の判断で実行して
//!   もらう設計とした。
//! - `gateway-connect`: ローカル側(Windows等)。TUNインターフェース
//!   を作成し、ボンディング接続の相手(`gateway-serve`)との間で
//!   IPパケットを中継する。**デフォルトルートをTUN経由に切り替える
//!   処理もこのプロセス自身では行わない**——誤った経路変更は
//!   接続断につながるため、必要なコマンド例をドキュメントに明記する
//!   に留める。
//!
//! ## 正直な開示(最重要)
//!
//! 1. **この開発環境(サンドボックス)には管理者権限・`wintun.dll`が
//!    無いため、実際のTUNインターフェース作成・実パケット転送の
//!    実機検証はできていない**。Windows側は`wintun.dll`
//!    (https://wintun.net/ 配布、実行ファイルと同じディレクトリへ
//!    配置)+管理者権限が必須、Linux側はTUN作成に`CAP_NET_ADMIN`
//!    (通常はroot)が必要。
//! 2. パケット単位のフレーミング(`framed::write_frame`/`read_frame`)
//!    自体は、`main.rs`の`serve`/`connect`と全く同じ実装・同じ
//!    ユニットテストで既に実証済みのロジックを再利用しているため、
//!    「1パケット=1フレーム」の往復が正しく機能すること自体は
//!    間接的に検証済みと言える。しかし実TUNデバイス経由の
//!    エンドツーエンド検証は次回、実機(管理者権限のあるWindows/Linux
//!    マシン)で行う必要がある。

use crate::accel::PayloadAccelerator;
use crate::framed;
use crate::qos::Qos;
use crate::quality::QualityTracker;
use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Instant;
use tun_rs::{AsyncDevice, DeviceBuilder};

/// TUNデバイスを作成する(IPv4アドレス+プレフィクス長のみ、IPv6は今回スコープ外)。
pub fn create_tun_device(addr: Ipv4Addr, prefix: u8, mtu: u16) -> Result<Arc<AsyncDevice>> {
    let dev = DeviceBuilder::new()
        .ipv4(addr, prefix, None)
        .mtu(mtu)
        .build_async()
        .context("failed to create TUN device (requires admin/root privileges and, on Windows, wintun.dll next to the executable)")?;
    tracing::info!(name = ?dev.name(), %addr, prefix, mtu, "TUN device created");
    Ok(Arc::new(dev))
}

/// TUNデバイスとボンディング接続(`agg_stream`)の間で、IPパケット単位で
/// 双方向に中継する。パケット1個 = フレーム1個(圧縮+暗号化)。
/// `quality`(RS-SmartTCP)へフレーム到達間隔を記録し、呼び出し元が
/// 切断時の再接続バックオフをネットワーク品質に応じて調整できる
/// ようにする(`quality.rs`と同じ「粗い近似」の設計、モジュールdoc
/// 参照)。
///
/// `qos`(オプトイン、`None`なら無制限)が指定されている場合、通過する
/// DNS応答を覗き見てストリーミング系サービスのIPを分類し、そのIPが
/// 絡むパケットだけ帯域を絞る。それ以外のトラフィックは同時にやり
/// 取りしていても無制限のまま(`qos.rs`モジュールdoc参照)。
pub async fn relay_packets(
    tun: Arc<AsyncDevice>,
    agg_stream: aggligator::alc::Stream,
    accel: Arc<PayloadAccelerator>,
    mtu: usize,
    quality: Arc<QualityTracker>,
    qos: Option<Arc<Qos>>,
) -> Result<()> {
    let (mut agg_rd, mut agg_wr) = tokio::io::split(agg_stream);

    let tun_to_agg = {
        let tun = Arc::clone(&tun);
        let accel = Arc::clone(&accel);
        let qos = qos.clone();
        async move {
            let mut buf = vec![0u8; mtu];
            loop {
                let n = tun.recv(&mut buf).await.context("reading packet from TUN device")?;
                if let Some(qos) = &qos {
                    qos.classifier.observe_ipv4_packet(&buf[..n]);
                    if let Some(dst) = crate::qos::packet_dest_ipv4(&buf[..n]) {
                        if qos.classifier.is_classified(dst) {
                            qos.limiter.consume(n).await;
                        }
                    }
                }
                framed::write_frame(&mut agg_wr, &accel, &buf[..n]).await?;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    let agg_to_tun = async move {
        loop {
            let started = Instant::now();
            match framed::read_frame(&mut agg_rd, &accel).await? {
                Some(packet) => {
                    quality.record_round_trip(started);
                    if let Some(qos) = &qos {
                        qos.classifier.observe_ipv4_packet(&packet);
                        if let Some(src) = crate::qos::packet_src_ipv4(&packet) {
                            if qos.classifier.is_classified(src) {
                                qos.limiter.consume(packet.len()).await;
                            }
                        }
                    }
                    tun.send(&packet).await.context("writing packet to TUN device")?;
                }
                None => break,
            }
        }
        anyhow::Ok(())
    };

    tokio::try_join!(tun_to_agg, agg_to_tun)?;
    Ok(())
}
