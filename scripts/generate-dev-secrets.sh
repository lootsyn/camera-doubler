#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SECRETS_DIR="$ROOT_DIR/secrets"
mkdir -p "$SECRETS_DIR"
command -v openssl >/dev/null || { echo "openssl is required" >&2; exit 1; }
umask 077

if [[ ! -s "$SECRETS_DIR/srt_passphrase.txt" ]]; then
  openssl rand -base64 32 | tr -d '\n' > "$SECRETS_DIR/srt_passphrase.txt"
fi
if [[ ! -s "$SECRETS_DIR/srt_streamid_hmac_key.bin" ]]; then
  openssl rand 32 > "$SECRETS_DIR/srt_streamid_hmac_key.bin"
fi
if [[ ! -s "$SECRETS_DIR/edge_control_ca.key" ]]; then
  openssl req -x509 -newkey rsa:3072 -nodes -days 3650 \
    -subj "/CN=robot-multicam-dev-ca" \
    -keyout "$SECRETS_DIR/edge_control_ca.key" \
    -out "$SECRETS_DIR/edge_control_ca.pem" >/dev/null 2>&1
fi
make_cert() {
  local name="$1" cn="$2" ext="$3"
  local key="$SECRETS_DIR/${name}.key" csr="$SECRETS_DIR/${name}.csr" crt="$SECRETS_DIR/${name}.crt"
  [[ -s "$crt" && -s "$key" ]] && return
  openssl req -new -newkey rsa:3072 -nodes -subj "/CN=${cn}" -keyout "$key" -out "$csr" >/dev/null 2>&1
  printf '%b\n' "$ext" > "$SECRETS_DIR/${name}.ext"
  openssl x509 -req -days 825 -sha256 -in "$csr" \
    -CA "$SECRETS_DIR/edge_control_ca.pem" -CAkey "$SECRETS_DIR/edge_control_ca.key" -CAcreateserial \
    -extfile "$SECRETS_DIR/${name}.ext" -out "$crt" >/dev/null 2>&1
  rm -f "$csr" "$SECRETS_DIR/${name}.ext"
}
make_cert edge_control_server "robot-edge" $'subjectAltName=DNS:robot-host,DNS:robot-edge,IP:127.0.0.1\nextendedKeyUsage=serverAuth'
make_cert edge_control_client "robot-receiver" 'extendedKeyUsage=clientAuth'
chmod 600 "$SECRETS_DIR"/*
echo "development secrets created under $SECRETS_DIR"
echo "Do not use these development certificates in production."
