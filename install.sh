#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="qcom-batt-guard"
BIN_NAME="qcom-batt-guard"

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
UNIT_SRC="${ROOT_DIR}/systemd/${SERVICE_NAME}.service"
UNIT_DST="/etc/systemd/system/${SERVICE_NAME}.service"
BIN_DST="/usr/local/sbin/${BIN_NAME}"

if [[ ! -f "$UNIT_SRC" ]]; then
  echo "ERROR: unit file not found: $UNIT_SRC" >&2
  exit 1
fi

echo "[1/5] Build release..."
cd "$ROOT_DIR"
cargo build --release

echo "[2/5] Install binary to ${BIN_DST}..."
sudo install -m 0755 "target/release/${BIN_NAME}" "${BIN_DST}"

echo "[3/5] Install systemd unit..."
sudo install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"

echo "[4/5] Reload unit and (re)activate service..."
sudo systemctl daemon-reload
sudo systemctl enable "${SERVICE_NAME}.service"
sudo systemctl restart "${SERVICE_NAME}.service"

echo "[5/5] Done."
echo "Status:"
sudo systemctl status "${SERVICE_NAME}.service" --no-pager || true
echo
echo "Logs:"
echo "  sudo journalctl -u ${SERVICE_NAME}.service -f"
