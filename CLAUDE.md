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
7. ~~GUI/サービス化~~ **一部完了(2026-07-23、後続HANDOFF参照)**:
   `gateway-serve`/`gateway-connect`(TUNゲートウェイ)・QoS・速度測定
   ・GUIを追加実装。ただしいずれも実機検証は未完了(下記参照)。
8. **次に優先すべきこと**: (a) タグpush(`v0.1.0`等)による
   `release.yml`の実動作確認、(b) 複数物理NIC・管理者権限を持つ実機
   でのTUNゲートウェイ・QoS・GUIの実機検証(本セッションはサンドボックス
   の制約で未完了)、(c) ~~GPU/NPUアクセラレーション~~
   **`open-cuda`側でGPU実装候補が完成(2026-07-23、下記追記参照)**、
   `accel.rs::AccelBackend::Gpu`への統合は次回セッションで着手。

- **2026-07-23(続き) `open-cuda`側でGPU圧縮/暗号化カーネル(ChaCha20)の
  実装が完了、`accel.rs::AccelBackend::Gpu`統合の実装候補ができた**:
  `open-cuda`の`opencuda-directx`クレートにChaCha20 GPUカーネル
  (DXIL/HLSL)が実装され、RustCrypto製`chacha20`クレートとの数値一致を
  実機(NVIDIA GT 730)で検証済み(コミット`ec6acf1`、詳細は`open-cuda`
  側CLAUDE.md HANDOFF参照)。**正直な開示・残作業**: (a) これは
  ChaCha20暗号化部分のみで、`accel.rs`が使う完全なAEAD
  (ChaCha20-Poly1305)には認証タグ(Poly1305)のGPU実装が別途必要、
  (b) 小サイズペイロード(トンネルのMTU程度、数百〜数千バイト)での
  H2D/D2Hオーバーヘッドが、GPU演算の優位性を相殺してしまわないかの
  実ベンチマークが未実施、(c) 本リポジトリ側の`AccelBackend::Gpu`は
  依然として`Cpu`へのフォールバックのみ(実際の配線はまだ行っていない)。
  次回セッションでの着手事項として記録。

## HANDOFF

- **2026-07-23 GPUバックエンドの実装完了・実機検証・セキュリティ修正**:
  - `opencuda-directx`(DirectX 12 Compute)バックエンドを`AccelBackend::Gpu`
    に統合し、ChaCha20暗号化をGPUオフロード可能にした(`--accel gpu`で
    `serve`/`connect`/`gateway-serve`/`gateway-connect`全サブコマンドから
    選択可能)。
  - **訂正(このマシンでの実機検証結果に基づく)**: 一時、このHANDOFFに
    「GT730はDirectX 12非対応のためCPUフォールバックする」という誤った
    記述があったが、事実と異なる。**GT 730はDirectX 12に対応している
    (Feature Level 11_0)**——`open-cuda`側のセッションで
    `D3D12CreateDevice`の実機成功・DXGIアダプタ列挙での
    `"NVIDIA GeForce GT 730"`取得・ChaCha20/matmul/vector_addの実GPU
    ディスパッチとCPU参照実装との完全一致を複数のテストで検証済み
    (詳細は`open-cuda`側`CLAUDE.md`のHANDOFF参照)。GT730が対応しない
    のはDirectX 12の新しいFeature Level(12_x系、Ray Tracing等)であり、
    「DirectX 12非対応」という表現は誤り。
  - **実重大バグの発見・修正**: 当初のGPU実装は認証タグ(Poly1305)を
    計算しておらず、GPUバックエンド選択時に改ざん検知が効かない
    (`open()`が改ざんデータを受理してしまう)という実質的な脆弱性が
    あった。RFC 8439のAEAD構成(counter=0ブロックからPoly1305一時鍵を
    導出、実データはcounter=1から暗号化)をCPU側`poly1305`crateで
    実装し、GPU(ChaCha20部分)と組み合わせることで解消。この構成が
    `chacha20poly1305`crateの出力と完全一致することをテストで検証済み
    (`gpu_poly1305_construction_matches_chacha20poly1305_reference`)。
    GPUバックエンドでの改ざんフレーム拒否も実機で確認済み
    (`gpu_backend_tampered_frame_is_rejected_if_available`)。
  - Vulkanバックエンド追加は現時点で必須ではない(DirectX 12で
    GT730含め動作確認済みのため)。将来的にmacOS/Linux/Android等
    非Windows環境でGPU加速したい場合の選択肢として残る。

- **2026-07-23(続き) TUNゲートウェイ・QoS・速度測定・GUI・自動再接続を
  追加(ユーザー指示、複数回にわたる追加要望を反映)**:
  1. **TUN仮想アダプタ方式のフルVPNゲートウェイ**(`src/tun_gateway.rs`、
     `gateway-serve`/`gateway-connect`サブコマンド)。`tun-rs`クレート
     (Windows: `wintun.dll`、Linux: カーネルTUN)でIPパケット単位の
     捕捉・注入を行い、既存の`framed`(圧縮+暗号化)をパケット単位で
     再利用。IPフォワーディング/NAT・デフォルトルート切替は自動化せず
     手動設定前提と明記(README.md参照)——OS設定の無断書き換えを
     避けるため。
  2. **自動再接続(ユーザー指示「WANとLANとWiFiのミックス対応接続は
     システム側で変化があっても自動で接続状況確認、自動最適調整で、
     自動対応」)**: `run_gateway_connect`を無限ループ化し、
     `RS-SmartTCP::AdaptivePolicy::retry_backoff()`で再試行間隔を
     品質に応じて自動調整。個々の物理インターフェースの増減自体は
     `aggligator::TcpConnector::link_tags`が内部で10秒間隔で自動
     再走査するため、このループは「ボンディング接続そのものが完全に
     切断された場合」の再確立を担う。
  3. **QoS(HiEndオーディオ向け帯域制御、ユーザー指示)**: `src/qos.rs`。
     DNS応答(UDP/53)スヌーピングでNetflix/U-NEXT/YouTube/Qobuz等の
     IPを分類し、そのトラフィックだけトークンバケットで帯域制限
     (既定10Mbps)、それ以外は同時アクセスでも無制限。`--qos-config`
     で任意有効化(既定オフ)、`default`で内蔵プリセット。**開発中に
     実バグを発見・修正**: `RateLimiter::consume()`がバースト容量を
     超える単発リクエストで永久にハングする欠陥があり、`cargo test`
     が実際にハングして発覚(型チェックのみで「完了」としない方針が
     機能した具体例)。トークンの初期値をバースト容量分で満たす標準的な
     設計に修正し、ハング再現テストを追加して検証済み。
  4. **ネット速度測定・自動記録**(ユーザー指示「自動測定・自動記録」):
     `src/speedtest.rs`。M-Lab(`ndt7`プロトコル、Google検索の速度
     テストと同じ基盤、`ndt7-client`クレート)のみ自動化——gate02/
     osakagas等の非公式サイトはユーザー確認の上、自動化せず手動記録
     機能(`speedtest record-manual`)に留めた(利用規約違反・破損
     リスクを避けるため)。測定時のネットワーク環境(インターフェース
     数・有線/無線内訳)を自動検出して併記。`speedtest prune`で古い
     記録の確認付き一括削除。
  5. **GUI**(ユーザー指示「速度測定というボタンを押したら実行される
     ように」「押さなければ良いように」): `src/gui.rs`、`egui`/
     `eframe`(Tauriには依存しない、既存エコシステム方針を踏襲)。
     ボタン押下=同意、確認ダイアログなし。「自動測定」チェックボックス
     で1時間ごとの無人測定を有効化。`gui` Cargo feature(既定オン)。
  6. **検証**: `cargo build`/`cargo test`ともgreen(10件、新規5件は
     `qos.rs`)。**GUIの実機検証は限定的**: デバッグログで実GPU
     (NVIDIA GT 730)でのOpenGL 3.3コンテキスト生成・ウィンドウ作成
     成功を確認したが、この開発環境の画面キャプチャ制限により実際の
     見た目・ボタン操作の目視確認はできなかった(正直な限界)。
     TUNゲートウェイ・QoSは実TUNデバイス経由の実機検証は未実施
     (管理者権限・複数物理NICが無いサンドボックス環境の制約)。
  7. **GPU/NPUアクセラレーション調査**(ユーザー指示「open-cudaも
     活かせたら活かして」「DirectXのプラグインとして」): `open-cuda`
     を調査した結果、実際はVulkan Compute基盤でDirectXへの依存は
     無く、圧縮・暗号化カーネルも存在しないことが判明。ユーザーは
     DirectX版への仕切り直しを希望したが、`aruaru-llm`への影響も
     及ぶ大きな方針転換のため、**次回はopen-cuda専用セッションで
     着手する**方針とし、`open-cuda`側CLAUDE.mdへ引き継ぎメモを
     記録・push済み(このリポジトリ側でのGPUアクセラレーション実装は
     今回未着手のまま)。

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
