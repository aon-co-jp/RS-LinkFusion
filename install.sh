#!/bin/sh
# RS-LinkFusion インストールスクリプト(AlmaLinux/Ubuntu/Debian/Fedora/RHEL等、
# systemdを使う主要Linuxディストリ共通)。
#
# **正直な開示**: このバイナリはUbuntu 20.04以降・Debian 11以降・
# AlmaLinux 8以降等、比較的新しいglibcを持つディストリ向けにビルド
# されている(muslによる完全なディストリ非依存の静的リンクは対象外、
# 詳細は .github/workflows/release.yml のコメント参照)。
#
# 使い方:
#   curl -fsSL https://github.com/aon-co-jp/RS-LinkFusion/releases/latest/download/rs-linkfusion-linux-x86_64.tar.gz | tar xz
#   sudo ./install.sh

set -eu

BIN_SRC="$(dirname "$0")/rs-linkfusion"
INSTALL_DIR="/usr/local/bin"
SERVICE_FILE="/etc/systemd/system/rs-linkfusion.service"

if [ "$(id -u)" -ne 0 ]; then
    echo "root権限で実行してください(例: sudo ./install.sh)" >&2
    exit 1
fi

if [ ! -f "$BIN_SRC" ]; then
    echo "rs-linkfusion バイナリが見つかりません($BIN_SRC)。同梱のtar.gzを展開したディレクトリで実行してください。" >&2
    exit 1
fi

echo "==> バイナリを ${INSTALL_DIR}/rs-linkfusion へ配置"
install -m 755 "$BIN_SRC" "${INSTALL_DIR}/rs-linkfusion"

if [ ! -f "$SERVICE_FILE" ]; then
    echo "==> systemdサービスのひな形を作成(${SERVICE_FILE}、既定では無効のまま)"
    cat > "$SERVICE_FILE" << EOF
[Unit]
Description=RS-LinkFusion - 複数WAN/LAN/WiFiボンディング通信トンネル
After=network.target

[Service]
Type=simple
# serve側(実サービスがあるマシン)の例:
#   ExecStart=${INSTALL_DIR}/rs-linkfusion serve --bind 0.0.0.0:5900 --target 127.0.0.1:8080 --key <rs-linkfusion generate-keyで生成した鍵>
# connect側(ローカル)の例:
#   ExecStart=${INSTALL_DIR}/rs-linkfusion connect --listen 127.0.0.1:8080 --remote <serve側のホスト名> --remote-port 5900 --key <同じ鍵>
# 上記どちらかをコメント解除・編集してから `systemctl enable --now rs-linkfusion` すること。
ExecStart=${INSTALL_DIR}/rs-linkfusion generate-key
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
else
    echo "==> 既存のsystemdサービスが見つかったため上書きしません(${SERVICE_FILE})"
fi

echo "==> 完了。まず鍵を生成し、次にserve/connectどちらの役割かに応じて設定してください:"
echo "    ${INSTALL_DIR}/rs-linkfusion generate-key"
echo "    sudo systemctl edit rs-linkfusion  # ExecStart を実際のserve/connectコマンドへ書き換え"
echo "    sudo systemctl enable --now rs-linkfusion"
