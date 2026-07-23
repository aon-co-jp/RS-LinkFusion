# RS-LinkFusion

**開発開始日: 2026-07-23**(このリポジトリのGitHub作成日)


- **GPU アクセラレーション（Vulkan 対応検討中）**:
  現在、GPU バックエンドは DirectX 12 に対応しています。ただし、GT730 のような DirectX 12 非対応 GPU では CPU フォールバック（`backend=Cpu`）で動作します。  
  GT730 は Vulkan 1.0 に対応しているため、将来のアップデートで Vulkan バックエンドを追加し、より多くの GPU で高速化を実現する予定です（ロードマップ項目）。


複数のWAN/LAN/WiFi(古い規格〜WiFi 7まで、OSがネットワークインター
フェースとして認識するものはすべて対象)を1つの論理接続へ束ね
(ボンディング)、通信の高速化・安定化を実現するアプリ。ポート単位の
軽量トンネル(`serve`/`connect`)と、PC上のあらゆる通信を束ねる
TUN仮想アダプタ方式のフルVPNゲートウェイ(`gateway-serve`/
`gateway-connect`)の2方式を提供する。インストーラー付きWindows/
Linux版として配布予定。

## 導入前におすすめすること

**RS-LinkFusion導入前に、以下いずれかで現在のネット速度を測定し、
記録を取っておかれることをお勧め致します**(導入後との比較用)。

- [Google検索の速度テスト](https://www.google.com/search?q=%E3%83%8D%E3%83%83%E3%83%88%E9%80%9F%E5%BA%A6%E6%B8%AC%E5%AE%9A)(M-Lab/ndt7ベース)
- [gate02 速度測定](https://speedtest.gate02.ne.jp/)
- [osakagas speedcheck](https://speedcheck.osakagas.co.jp/#!/?=true)

このうちM-Lab(Google検索の速度テストと同じ基盤)は`rs-linkfusion
speedtest run`で自動測定・自動記録できる(下記「ネット速度測定」参照)。
gate02・osakagasは非公式サイトのため自動化しておらず、手動で開いて
読み取った値を`rs-linkfusion speedtest record-manual`で同じ履歴へ
記録できる。

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
- **自動再接続・自動最適化**: システム側でインターフェース構成が
  変化しても(LANケーブル抜き差し・WiFi接続/切断等)、`aggligator`が
  10秒間隔で自動的に再走査し追従する。ボンディング接続そのものが
  完全に切断された場合も、`gateway-connect`が[RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP)
  の適応バックオフ(実測RTT/ジッターに応じてFast/Slow切替)で自動的に
  再接続を試み続ける。
- **CPU/GPU/NPU/専用ハードウェアアクセラレータの抽象化**: `AccelBackend`
  列挙型(`Cpu`/`Gpu`/`Npu`/`HardwareAccelerator`)で将来のハードウェア
  をAPI形状として先取りし、現状`Cpu`のみ実装(`flate2`圧縮+
  ChaCha20-Poly1305暗号化)、他は要求時に安全に`Cpu`へフォールバック
  する。GPU/NPU実装は調査済みだが未着手(下記「正直な開示」参照)。
- **トンネル方式**: `[len:u32 LE][圧縮+暗号化済みペイロード]`という
  長さプレフィクスフレームで、ボンディング接続上にトラフィックを流す。
  `serve`/`connect`は固定1アドレスへのポート転送、`gateway-serve`/
  `gateway-connect`はTUN仮想アダプタでIPパケット単位のフルVPN。
- **QoS(HiEndオーディオ向け帯域制御、オプトイン)**: 動画配信/VOD/
  音楽配信サービス(Netflix・U-NEXT・YouTube・Qobuz等)向けの通信だけ
  帯域上限(既定10Mbps)をかけ、それ以外のダウンロード/アップロードは
  同時アクセスでもボンディング接続の実効速度まで無制限、という2層
  構成を選択制で提供する(`qos.rs`)。
- **ネット速度測定・自動記録**: [M-Lab](https://www.measurementlab.net/)
  (`ndt7`プロトコル、Google検索の速度テストと同じ基盤)で速度測定し、
  測定時点のネットワーク環境(インターフェース数・有線/無線の内訳)と
  併せてJSONL形式で記録する。`speedtest watch`で確認なしの定期自動
  測定も可能。
- **GUI(「速度測定」ボタン)**: `egui`/`eframe`製の最小限のウィンドウ
  (Tauriには依存しない、既存のエコシステム方針を踏襲)。ボタンを押す
  ことが同意——押さなければ測定は一切実行されない。

## 使用例(ポート転送モード)

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

## 使用例(TUNゲートウェイ・フルVPNモード)

管理者権限が必要。Windowsでは[wintun.dll](https://wintun.net/)を
実行ファイルと同じディレクトリに配置すること。

```bash
# リモート側(典型的にはLinux VPS)
sudo rs-linkfusion gateway-serve --bind 0.0.0.0:5900 --key <鍵>
# QoSプリセット(動画/音楽配信を10Mbpsへ制限)を有効にする場合:
sudo rs-linkfusion gateway-serve --bind 0.0.0.0:5900 --key <鍵> --qos-config default

# ローカル側(Windows、管理者権限のPowerShellで)
rs-linkfusion gateway-connect --remote <serve側のIP> --remote-port 5900 --key <同じ鍵>
```

**正直な開示**: TUN作成後のIPフォワーディング/NAT(serve側、Linux)・
デフォルトルートのTUN経由への切り替え(connect側)は、このアプリ自身
では自動化していない(誤設定時の影響が大きいため、手動設定を前提と
する)。serve側の例(Linux、要root):

```bash
sysctl -w net.ipv4.ip_forward=1
iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE  # eth0は実際のWANインターフェース名に置き換え
```

connect側(Windows)でデフォルトルートをTUN経由に切り替える例:

```powershell
route add 0.0.0.0 mask 0.0.0.0 10.66.0.1 metric 1
```

## ネット速度測定

```bash
# 1回測定(M-Lab、対話的に同意確認)
rs-linkfusion speedtest run --label baseline

# 確認なしで1回測定(スクリプト等から)
rs-linkfusion speedtest run --label accelerated --yes

# 1時間ごとに確認なしで自動測定・自動記録し続ける(Ctrl+Cで終了)
rs-linkfusion speedtest watch --interval-minutes 60

# gate02/osakagas等、非公式サイトを手動で開いて読み取った値を記録
rs-linkfusion speedtest record-manual --source gate02 --download-mbps 350 --upload-mbps 120

# 履歴を表示
rs-linkfusion speedtest history

# 90日より古い記録を確認のうえまとめて削除
rs-linkfusion speedtest prune --older-than-days 90
```

## GUI

```bash
rs-linkfusion gui
```

「速度測定」ボタンを押すと測定・記録が実行される(押さなければ何も
起きない)。「自動測定」チェックボックスで1時間ごとの無人測定・記録
を有効化できる。

## 正直な開示

- 個々の物理リンク単位の内訳ではなく、ボンディングされた論理接続
  全体の実効品質という単純化を採用している(`quality.rs`)。
- **GPU/NPUアクセラレーションは未実装**。関連リポジトリ`open-cuda`
  (GPU抽象化基盤)を調査したが、現状はVulkan Compute基盤でML専用
  カーネル(GEMM/Attention等)のみを持ち、圧縮・暗号化カーネルは
  存在しないため転用できない。トンネル1フレームは小サイズ(MTU程度)
  のため、Host↔Device間の転送オーバーヘッドがGPU側の演算優位性を
  相殺し実利益が出ない可能性がある、という技術的懸念も判明している
  (詳細は`open-cuda`側`CLAUDE.md`のHANDOFF参照)。
- **QoSのサービス分類はDNS応答スヌーピングによるベストエフォート**。
  CDN・エニーキャストIPは複数サービスで共有されることがあるため、
  分類の精度は完全ではない(`qos.rs`参照)。
- **TUNゲートウェイは複数クライアント同時接続を想定していない**
  (1つのTUNデバイスに対し単一クライアント前提)。
- macOS/Android/iOS/スマートTV対応は計画のみ(詳細は`CLAUDE.md`参照)。
- **この開発環境では管理者権限・複数物理NIC・実際のTUNドライバでの
  実機検証ができていない**(サンドボックス環境の制約)。ループバック
  上でのポート転送モードの実データ往復は実機検証済み。GUIはウィンドウ
  作成・実GPU(OpenGL)コンテキスト生成の成功をログで確認済みだが、
  この環境の画面キャプチャ制限により見た目の目視確認はできていない。

## 対応プラットフォーム

| プラットフォーム | 状況 |
|---|---|
| Windows | 主要ターゲット。`install.ps1`で導入可能。TUNゲートウェイには`wintun.dll`が必要 |
| Linux | 主要ターゲット。`install.sh`で導入可能 |
| macOS/Android/iOS/スマートTV | 計画のみ(詳細は`CLAUDE.md`参照) |

## このエコシステムでの関連

- [RS-SmartTCP](https://github.com/aon-co-jp/RS-SmartTCP) — ネットワーク
  品質適応制御・自動再接続バックオフの利用元。
- [open-web-server](https://github.com/aon-co-jp/open-web-server) —
  `accel.rs`/`aggligator`利用パターンの原型
  (`open-web-server-wire::accel`/`mptcp_channel`)。
- [open-cuda](https://github.com/aon-co-jp/open-cuda) — GPU抽象化基盤
  (現状Vulkan Compute、DirectX版への方針転換をユーザーが検討中、
  詳細は同リポジトリのCLAUDE.md HANDOFF参照)。

## ビルド・テスト

```bash
cargo build
cargo test
```

GUIを含めない場合(`gui` featureは既定で有効):

```bash
cargo build --no-default-features
```

## ライセンス

Apache-2.0
