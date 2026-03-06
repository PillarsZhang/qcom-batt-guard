#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="qcom-batt-guard"
BIN_NAME="qcom-batt-guard"

UNIT_DST="/etc/systemd/system/${SERVICE_NAME}.service"
BIN_DST="/usr/local/sbin/${BIN_NAME}"

echo "[1/3] Disable & stop service..."
sudo systemctl disable --now "${SERVICE_NAME}.service" 2>/dev/null || true

echo "[2/3] Remove unit and reload systemd..."
sudo rm -f "${UNIT_DST}"
sudo systemctl daemon-reload

echo "[3/3] Remove binary..."
sudo rm -f "${BIN_DST}"

echo "Uninstalled ${SERVICE_NAME}."
