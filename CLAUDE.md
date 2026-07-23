# 開発方針・設計書(RS-LinkFusion) — 2026-07-23時点、実装未検証

> ⚠️ **正直な開示(最重要)**: このファイルは**設計書**であり、
> `src/`配下のRustコードは書き上げた段階で`cargo build`/`cargo test`
> による検証を行えていない(リミット接近のため中断)。次回セッションで
> 必ず最初に実施すること: `cargo test`を実行し、コンパイルエラー・
> テスト失敗を修正してから先へ進む(型チェックのみで「完了」と
> 報告しないという、このエコシステム共通の方針を厳守)。

作業ドライブは`F:\runo`。この節は[`open-raid-z`](https://github.com/aon-co-jp/open-raid-z)の
`CLAUDE.md`を正本とし、各プロジェクトへコピーして同期する方針に準じる。

## このプロジェクトの役割

複数のWAN/LAN/WiFi(古い規格〜WiFi 7まで、OSがネットワークインター
フェースとして認識するものはすべて対象)を1つの論理接続へ束ね
(ボンディング)、通信の高速化・安定化を実現する、インストーラー付き
Windows/Linuxアプリ。ユーザー指示「上記通信技術で、CPU＋GPU+NPUが
あればハードウェアアクセラレーター可能として。複数のWANの複数の
LAN＋複数のWiFiは、古いWiFiからWifi7まで対応の融合(ミックス)を可能
として、通信の高速化と安定化などを可能として、インストーラー付き
Windows版とLINUX版アプリとしても、提供してダウンロード可能にして」
に基づく。

## 設計の核心

### 1. 複数インターフェースのボンディング(実装済み・実績のあるライブラリを採用)

`aggligator`(`aggligator-transport-tcp`)クレートを採用。このリポジトリ
自身が新たに実装するのではなく、**既に`open-web-server-wire::mptcp_channel`
で実績のある枯れたクレート**を使う。日英Web検索で裏取り済みの重要な
発見:

- `TcpConnector::set_multi_interface(true)`(**デフォルトで有効、
  Android以外**)が、ローカルマシンが持つ**全ネットワークインター
  フェース**(LAN・WiFi・複数WAN等、OSが認識するものすべて)を自動
  列挙し、各インターフェース×各サーバーIPの組み合わせごとに個別の
  TCPリンクを確立、それらを1つの論理接続へ束ねる。**この機能は
  「LAN」「WiFi」「WAN」を区別しない**——単に「ローカルにあるすべての
  ネットワークインターフェース」を対象にするため、古いWiFi規格〜
  WiFi 7まで、OSがIPを持つインターフェースとして認識してさえいれば
  そのまま対象になる(WiFi規格自体の違いはOS/NICドライバ層で吸収
  される、アプリケーション層で規格ごとの特別対応は不要)。
- 実世界の直接の先行例: **Speedify**(複数のWAN/WiFi/セルラー回線を
  束ねる商用ソフト、Windows/macOS/Linux/iOS/Android対応、専用
  ハードウェア不要)。**OpenMPTCProuter**はカーネルMPTCP非対応環境で
  Glorytun/MLVPNを使う設計——このリポジトリがWindows(カーネルMPTCP
  非対応)で`aggligator`を使うのと同じ判断構造。
- 「品質連動型ルーティング」(ping応答があってもHTTP応答が遅い場合は
  即座にバックアップ回線へ切替)が2026年の到達点とされている
  ([jisaku.com: 回線冗長化・帯域結合](https://jisaku.com/posts/network-bonding-failover-2026))
  ——これを担うのが下記2.の`RS-SmartTCP`統合。

### 2. ネットワーク品質適応(`RS-SmartTCP`、実装済み・別リポジトリ)

[RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP)の
`NetworkQualityMonitor`/`AdaptivePolicy`をそのまま利用し、ボンディング
接続の実効RTT/ジッターを追跡、IOWN/APNのような光ネットワーク級の
リンクを検知した場合と通常インターネット級とで挙動を切り替える
(`quality.rs`)。**正直な開示**: 個々の物理リンク単位の内訳ではなく、
ボンディングされた論理接続全体の実効品質という単純化を採用した
(`aggligator::alc::Stream`が公開するリンク単位統計を使うより粗い
実装、次回以降の高度化候補)。

### 3. CPU/GPU/NPU/専用ハードウェアアクセラレータの抽象化(`accel.rs`)

`open-web-server-wire::accel`と同じ設計判断——`AccelBackend`列挙型
(`Cpu`/`Gpu`/`Npu`/`HardwareAccelerator`)で将来のハードウェアをAPI
形状として先取りし、**`Cpu`のみ実装**(`flate2`圧縮+ChaCha20-Poly1305
暗号化)、他は要求時に安全に`Cpu`へフォールバックする。本リポジトリは
独立した配布物(ダウンロードしてすぐ動く単体バイナリ)であるため、
`open-web-server`への依存を持たせず、同じパターンを自己完結で再実装
した(コード重複はあるが、依存グラフを軽量に保つトレードオフ)。

日英Web検索での裏取り: GPU圧縮(NVIDIA nvCOMP、Snappy/ZSTD/LZ4対応、
Blackwell世代は専用デコンプレッションエンジンで600GB/s)・GPU暗号化
(CUDA上でのAES高速化、学術研究レベルで実例あり)は実在するが、Rust
エコシステムには両者を統合した実用クレートが見当たらず、今回は
CPUバックエンドのみを実装。

### 4. トンネル方式(`framed.rs`、実装済み・未検証)

`[len:u32 LE][圧縮+暗号化済みペイロード]`という単純な長さプレフィクス
フレームで、ボンディング接続上に任意のTCPトラフィックを流す
「ミニVPN的トンネル」。`serve`(リモート側、ローカルサービスへの
リバースプロキシ)と`connect`(ローカル側、ローカルポートで待ち受け
てボンディング接続へ転送)の2つのCLIサブコマンドを想定
(**未実装**、次回セッションで着手)。

## 対応プラットフォーム(現状・計画)

| プラットフォーム | 状況 |
|---|---|
| Windows | 今回の主要ターゲット。`install.ps1`想定(未作成) |
| Linux | 今回の主要ターゲット。`install.sh`想定(未作成) |
| macOS | **計画のみ**——Mac実機購入後に着手する前提(ユーザー確認済み)。インストーラー部分だけ別設計にする方針(ユーザー指示) |
| Android | **計画のみ**——Rust+Android NDKでのクロスビルドは技術的に可能性ありと考えられるが、本セッションではツールチェーン未確認・未着手 |
| iPhone/iPad(iOS/iPadOS) | **計画のみ**——ビルドにXcode(Mac必須)が必要、macOS対応と同じ制約 |
| スマートTV/4K TV | **計画のみ、スコープ判断を保留**。ユーザー指摘の通りLAN+WiFiを両方持つTVでは技術的にボンディングは意味を持ちうるが、「回線冗長化は本来ルーター/PC側で行うもの」という観点との整合、および対応OS(Android TV/Tizen/webOS)ごとのツールチェーン差異は次回セッションで要検討 |

## 未実装・次回セッションで最優先すべきこと

1. **`cargo build`/`cargo test`の実行**(最優先、型チェックのみで
   「完了」と報告しない方針の徹底)。`accel.rs`/`framed.rs`/
   `quality.rs`は実装済みだが未検証。
2. `main.rs`(CLIエントリポイント、`serve`/`connect`サブコマンド)の
   実装——現状は`src/`に個別モジュールがあるのみで、これらを束ねる
   `main.rs`自体が未作成。
3. 実際に複数インターフェースを持つマシンでの実機検証(この開発
   環境がループバックのみか複数NICを持つかの確認を含む)。
4. `install.sh`(Linux、`RS-Guard`/`RS-Ops`の既存パターンを踏襲)・
   `install.ps1`(Windows)の作成。
5. GitHub Releases経由でのビルド済みバイナリ配布(タグpushで
   Linux/Windows向け自動ビルド、既存の`.github/workflows/release.yml`
   パターンを踏襲)。
6. README.md/PORTING.mdの作成(現状CLAUDE.mdのみ)。

## 関連プロジェクト

- [open-raid-z](https://github.com/aon-co-jp/open-raid-z) — 開発ルールの正本。
- [RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP) — ネットワーク品質適応制御の利用元。
- [open-web-server](https://github.com/aon-co-jp/open-web-server) — `accel.rs`/`mptcp_channel.rs`と同じ設計パターンの原型。
- [RS-Guard](https://github.com/aon-co-jp/RS-Guard) — インストーラー(install.sh/install.ps1)パターンの参照元。

## エコシステム全体マップ

同時並行開発の対象プロジェクト一覧・各リポジトリの現況は
[`open-raid-z`のCLAUDE.md](https://github.com/aon-co-jp/open-raid-z/blob/main/CLAUDE.md)
「関連プロジェクト」節を参照。
