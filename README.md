# RS-LinkFusion

**開発開始日: 2026-07-23**(このリポジトリのGitHub作成日)

複数のWAN/LAN/WiFi(古い規格〜WiFi 7まで、OSがネットワークインター
フェースとして認識するものはすべて対象)を1つの論理接続へ束ね
(ボンディング)、通信の高速化・安定化を実現するトンネルアプリ。
インストーラー付きWindows/Linux版として配布予定。

## これは何か

- **複数インターフェースのボンディング**: [`aggligator`](https://docs.rs/aggligator)
  (`aggligator-transport-tcp`)を採用。`TcpConnector::set_multi_interface(true)`
  (デフォルトで有効、Android以外)が、ローカルマシンが持つ**全ネット
  ワークインターフェース**(LAN・WiFi・複数WAN等、OSが認識するもの
  すべて)を自動列挙し、各インターフェース×各サーバーIPの組み合わせ
  ごとに個別のTCPリンクを確立、それらを1つの論理接続へ束ねる。
  「LAN」「WiFi」「WAN」を区別せず、古いWiFi規格〜WiFi 7まで、OSが
  IPを持つインターフェースとして認識してさえいればそのまま対象になる
  (WiFi規格自体の違いはOS/NICドライバ層で吸収される)。
- **ネットワーク品質適応**: [RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP)
  の`NetworkQualityMonitor`/`AdaptivePolicy`を利用し、ボンディング
  接続の実効RTT/ジッターを追跡する。
- **CPU/GPU/NPU/専用ハードウェアアクセラレータの抽象化**: `AccelBackend`
  列挙型(`Cpu`/`Gpu`/`Npu`/`HardwareAccelerator`)で将来のハードウェア
  をAPI形状として先取りし、現状`Cpu`のみ実装(`flate2`圧縮+
  ChaCha20-Poly1305暗号化)、他は要求時に安全に`Cpu`へフォールバック
  する。
- **トンネル方式**: `[len:u32 LE][圧縮+暗号化済みペイロード]`という
  長さプレフィクスフレームで、ボンディング接続上に任意のTCPトラ
  フィックを流す「ミニVPN的トンネル」。

## 使用例

```bash
# 鍵を1つ生成し、serve側・connect側で同じ値を使う
rs-linkfusion generate-key
# => 64桁のhex文字列

# リモート側(実サービスがあるマシン): ボンディング接続を受け付け、
# ローカルの実サービス(例: 127.0.0.1:8080)へリバースプロキシする
rs-linkfusion serve --bind 0.0.0.0:5900 --target 127.0.0.1:8080 --key <上記の鍵>

# ローカル側: ローカルポート(例: 127.0.0.1:8080)で待ち受け、
# serve側のボンディング接続へ転送する
rs-linkfusion connect --listen 127.0.0.1:8080 --remote <serve側のホスト名/IP> --remote-port 5900 --key <同じ鍵>
```

## 正直な開示

- 個々の物理リンク単位の内訳ではなく、ボンディングされた論理接続
  全体の実効品質という単純化を採用している(`quality.rs`)。
- GPU圧縮(NVIDIA nvCOMP等)・GPU暗号化は実在する技術だが、Rust
  エコシステムに両者を統合した実用クレートが見当たらず、現状は
  CPUバックエンドのみ実装。
- macOS/Android/iOS/スマートTV対応は計画のみ(詳細は`CLAUDE.md`参照)。

## 対応プラットフォーム

| プラットフォーム | 状況 |
|---|---|
| Windows | 主要ターゲット。`install.ps1`で導入可能 |
| Linux | 主要ターゲット。`install.sh`で導入可能 |
| macOS/Android/iOS/スマートTV | 計画のみ(詳細は`CLAUDE.md`参照) |

## このエコシステムでの関連

- [RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP) — ネットワーク
  品質適応制御の利用元。
- [open-web-server](https://github.com/aon-co-jp/open-web-server) —
  `accel.rs`/`aggligator`利用パターンの原型
  (`open-web-server-wire::accel`/`mptcp_channel`)。

## ビルド・テスト

```bash
cargo build
cargo test
```

## ライセンス

Apache-2.0
