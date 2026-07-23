# 開発方針・設計書(RS-LinkFusion) — 2026-07-23時点、コアロジック実装・実機検証済み

> ⚠️ **正直な開示**: 前回セッションはリミット接近で`cargo build`/
> `cargo test`未検証のまま中断していたが、本セッションで検証・
> `main.rs`実装・実データ転送の実機検証まで完了した(詳細は
> HANDOFF参照)。GUI/サービス化・macOS/Android/iOS対応は引き続き
> 未着手(ユーザー確認済みのスコープ、下記「対応プラットフォーム」
> 節参照)。

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

1. ~~`cargo build`/`cargo test`の実行~~ **完了(2026-07-23)**。
2. ~~`main.rs`の実装~~ **完了(2026-07-23)**。
3. **実際に複数インターフェースを持つマシンでの実機検証**は未着手
   (この開発環境がループバックのみか複数NICを持つかの確認を含む)。
   ループバック上でのserve/connect実データ転送検証は完了済み
   (下記HANDOFF参照)——複数物理NICでの真のマルチホーミング効果
   自体はこのサンドボックスでは検証できていない、という正直な限界。
4. ~~`install.sh`/`install.ps1`の作成~~ **完了(2026-07-23)**。
5. ~~GitHub Releases自動ビルドワークフロー~~ **完了(2026-07-23、
   `.github/workflows/release.yml`)**。タグpushでの実リリース動作
   自体は未検証。
6. ~~README.md/PORTING.mdの作成~~ **完了(2026-07-23)**。
7. **次に優先すべきこと**: (a) タグpush(`v0.1.0`等)による
   `release.yml`の実動作確認、(b) 複数物理NIC環境での実機検証、
   (c) GUI/サービス化(今回スコープ外、ユーザー確認済み)。

## HANDOFF

- **2026-07-23 コアロジック実装・実機検証完了**: 前回セッションが
  リミット接近で中断していた`cargo build`/`cargo test`未検証状態を
  解消。
  1. `cargo build`成功(警告2件のみ、`AccelBackend`の未実装
     バリアント`Gpu`/`Npu`/`HardwareAccelerator`が未使用という
     dead_code警告——`open-web-server-wire::accel`と同じ設計上
     意図的な未使用のため実害なし)。
  2. `cargo test`で既存5件(accel 3件・framed 2件)全green。
  3. `main.rs`を新規実装。`clap`で`generate-key`/`serve`/`connect`の
     3サブコマンドを提供。`serve`は`aggligator_transport_tcp::simple::
     tcp_server`でボンディング接続を受け付けローカルターゲットへ
     `TcpStream::connect`、`connect`は`tokio::net::TcpListener`で
     ローカル待受し、接続ごとに`simple::tcp_connect`でボンディング
     接続を新規に張る。両者とも`tokio::io::split`+`tokio::try_join!`
     による双方向リレー(`relay()`関数、`framed::write_frame`/
     `read_frame`でボンディング側を圧縮+暗号化)。鍵は64桁hex文字列
     で`serve`/`connect`双方に手動で渡す設計(`generate-key`
     サブコマンドで生成)。
  4. **実機検証(型チェックのみで完了と報告しない方針の徹底)**:
     Python製ループ型echoサーバー(127.0.0.1:9402)を用意し、
     `rs-linkfusion serve --bind 127.0.0.1:9501 --target 127.0.0.1:9402`
     と`rs-linkfusion connect --listen 127.0.0.1:9601 --remote 127.0.0.1
     --remote-port 9501`を実際に起動、ループバック上で実際に
     `aggligator`のリンクテスト(ping計測含む)が完了することを
     デバッグログで確認した上で、connect側のローカルポート
     (127.0.0.1:9601)へPythonクライアントから400バイト送信し、
     serve側経由でechoサーバーへ到達・折り返され、connect側から
     送信時と**完全に一致する400バイトが実際に返ってくる**ことを
     実TCPソケットで確認(`received 400 bytes / match: True`)。
     圧縮+暗号化フレーム化・ボンディング接続経由の往復・復号+解凍が
     実際に機能することを実証した。
     **正直な限界**: この開発環境はループバック(単一の仮想
     インターフェース「Loopback Pseudo-Interface 1」)のみのため、
     複数物理NICでの真のマルチホーミング効果自体は検証できていない
     (`aggligator`側のログでも単一インターフェースのみが列挙されて
     いることを確認済み)。
  5. `README.md`/`PORTING.md`を新規作成(3点セットが揃った)。
  6. `install.sh`/`install.ps1`を`open-web-server`の既存パターンから
     移植(サービスはTCP転送方式のため、環境変数ではなく
     `ExecStart`のコマンドライン引数でserve/connectを切り替える形に
     調整)。
  7. `.github/workflows/release.yml`を`open-web-server`の既存パターンから
     移植(タグpushでLinux/Windows向けバイナリ自動ビルド)。
     タグpushによる実リリース動作自体は未検証(次回優先事項)。
  - 次にすべきこと: 上記「未実装・次回セッションで最優先すべきこと」
    節を参照。

## 関連プロジェクト

- [open-raid-z](https://github.com/aon-co-jp/open-raid-z) — 開発ルールの正本。
- [RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP) — ネットワーク品質適応制御の利用元。
- [open-web-server](https://github.com/aon-co-jp/open-web-server) — `accel.rs`/`mptcp_channel.rs`と同じ設計パターンの原型。install.sh/install.ps1/release.ymlの参照元。

## エコシステム全体マップ

同時並行開発の対象プロジェクト一覧・各リポジトリの現況は
[`open-raid-z`のCLAUDE.md](https://github.com/aon-co-jp/open-raid-z/blob/main/CLAUDE.md)
「関連プロジェクト」節を参照。
