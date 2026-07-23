# PORTING.md — お引越し可能ファイル

他のプロジェクトへそのまま(または軽微な変更で)移植できる実装パターン
一覧。

## `aggligator`によるユーザー空間マルチホーミング(`src/main.rs`)

`aggligator-transport-tcp::simple::{tcp_connect, tcp_server}`は、
`TcpConnector::set_multi_interface(true)`(デフォルト有効)により
ローカルの全ネットワークインターフェースを自動列挙し、各インター
フェース×各サーバーIPごとに個別TCPリンクを張って1つの論理接続へ
束ねる。カーネルMPTCP/SCTPが使えない環境(Windows等)でも同じ目的
(物理経路の冗長化・帯域合算)をユーザー空間で実現できる、実績のある
パターン(`open-web-server-wire::mptcp_channel`が原型)。

```rust
use aggligator_transport_tcp::simple as agg_tcp;

// サーバー側
agg_tcp::tcp_server(bind_addr, |stream| async move {
    // streamは AsyncRead + AsyncWrite を実装する束ねられた論理接続
}).await?;

// クライアント側
let stream = agg_tcp::tcp_connect(["server.example.com".to_string()], 5900).await?;
```

## 長さプレフィクス付き圧縮+暗号化フレームプロトコル(`src/framed.rs`)

`[len:u32 LE][圧縮+暗号化済みペイロード]`という単純な形式で、任意の
`AsyncRead`/`AsyncWrite`ストリーム上に任意サイズのメッセージを安全に
送受信する最小実装。トンネル・RPC・ミニVPN等、ストリーム型プロトコル
の上に「1メッセージ単位の圧縮+暗号化された往復」が必要などんな場面
にも移植できる。

```rust
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W, accel: &PayloadAccelerator, plaintext: &[u8],
) -> anyhow::Result<()> {
    let sealed = accel.seal(plaintext)?;
    let len = (sealed.len() as u32).to_le_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&sealed).await?;
    writer.flush().await?;
    Ok(())
}
```

## 圧縮+暗号化ハードウェアアクセラレータ抽象化(`src/accel.rs`、移植元:
`open-web-server-wire::accel`、同パターンを自己完結で再実装)

`AccelBackend`列挙型で将来のハードウェア(GPU/NPU/専用アクセラレータ)
をAPI形状として先取りし、未実装のバックエンドが要求されてもpanicせず
`Cpu`へ安全にフォールバックしつつ`tracing::warn!`で可視化する設計。
呼び出し側のコードを変えずに将来ハードウェアが実装された時にそのまま
差し替わる。独立配布物(ダウンロードしてすぐ動く単体バイナリ)にする
場合は、この`open-web-server`への依存を持たせない自己完結実装のまま
コピーするのが移植コスト最小。

```rust
pub fn new(backend: AccelBackend, key: &[u8; 32]) -> Self {
    let effective = match backend {
        AccelBackend::Cpu => AccelBackend::Cpu,
        other => {
            tracing::warn!(requested = ?other, "accelerator backend not yet implemented, falling back to Cpu");
            AccelBackend::Cpu
        }
    };
    Self { backend: effective, cipher: ChaCha20Poly1305::new(Key::from_slice(key)) }
}
```

## ボンディング接続とローカルTCP接続の双方向リレー(`src/main.rs::relay`)

`tokio::io::split`で1本のストリーム(ボンディング接続)を読み書き
半分に分け、`tokio::try_join!`で双方向コピーを並行実行するパターン。
片方が`AsyncRead+AsyncWrite`を実装する任意のトンネル層、もう片方が
平文ローカルソケットという構図であれば、SSH的なポートフォワード・
リバースプロキシ全般にそのまま移植できる。

```rust
let (mut local_rd, mut local_wr) = local.into_split();
let (mut agg_rd, mut agg_wr) = tokio::io::split(agg_stream);

let to_agg = async { /* local_rd → framed::write_frame → agg_wr */ };
let to_local = async { /* framed::read_frame ← agg_rd → local_wr */ };
tokio::try_join!(to_agg, to_local)?;
```
### ハードウェアアクセラレータのフォールバックパターン（移植時の重要な教訓）

`accel.rs` の `PayloadAccelerator::new()` は、要求されたバックエンド（`AccelBackend::Gpu`）が利用できない場合に **自動的に CPU にフォールバック** する設計です。  
このパターンは、移植先の環境が GPU をサポートしていない場合でもアプリケーションが継続して動作することを保証します。

**移植時の注意**：
- GPU バックエンドを追加する場合、必ず `cfg(feature = "...")` で条件コンパイルし、フォールバック経路を用意してください。
- **訂正(2026-07-23、実機検証済み)**: 以前このファイルに「GT730はDirectX 12
  非対応」という誤った記述があったが、事実と異なる。**GT730はDirectX 12に
  対応している(Feature Level 11_0)**——`open-cuda`側`opencuda-directx`
  クレートで`D3D12CreateDevice`の実機成功・DXGIアダプタ列挙での
  `"NVIDIA GeForce GT 730"`取得・GPUディスパッチとCPU参照実装の完全一致を
  複数のテストで検証済み(詳細は`open-cuda`側`CLAUDE.md`のHANDOFF参照)。
  GT730が対応しないのはDirectX 12の新しいFeature Level(12_x系、Ray
  Tracing等)であり、「DirectX 12非対応」という表現自体が誤り。
  Vulkanバックエンドの追加は現時点で必須ではない(DirectX 12で
  GT730を含め動作確認済みのため)——非Windows環境(Linux/Android等)で
  GPU加速したい場合の選択肢として残る。
```