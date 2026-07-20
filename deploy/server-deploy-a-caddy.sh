#!/bin/bash
set -euo pipefail

REPO=/home/justin/pcsensor/web-share
OPT=/opt/chat-transfer

echo "==> 1. user + dirs"
if ! id chatxfer >/dev/null 2>&1; then
  useradd --system --home "$OPT" --shell /usr/sbin/nologin chatxfer
fi
mkdir -p "$OPT/data/uploads" "$OPT/static"
chown -R chatxfer:chatxfer "$OPT"

echo "==> 2. binary + static"
install -o chatxfer -g chatxfer -m 550 \
  "$REPO/target/release/chat-transfer" "$OPT/chat-transfer"
rm -rf "$OPT/static"
mkdir -p "$OPT/static"
cp -a "$REPO/static/." "$OPT/static/"
chown -R chatxfer:chatxfer "$OPT/static" "$OPT/data"
chmod 750 "$OPT"

echo "==> 3. env"
SECRET_KEY="$(openssl rand -hex 32)"
cat > "$OPT/env" <<EOF
# Chat Transfer production env — managed on server, do not commit
CHAT_BIND=127.0.0.1:8080
CHAT_SECRET_KEY=${SECRET_KEY}
CHAT_BOOTSTRAP_ADMIN_USER=admin
CHAT_BOOTSTRAP_ADMIN_PASSWORD=CHANGE_ME
CHAT_REGISTRATION_OPEN=true
CHAT_INVITE_TTL_HOURS=24
CHAT_DEVICE_TRUST_DAYS=15
CHAT_TOTP_ISSUER="Chat Transfer"
CHAT_DATA_DIR=/opt/chat-transfer/data
CHAT_MAX_FILE_MB=100
CHAT_MAX_MSG_LEN=4000
CHAT_MAX_HISTORY=2000
CHAT_RETENTION_SECS=10800
CHAT_PURGE_INTERVAL_SECS=60
CHAT_SESSION_TTL_SECS=86400
CHAT_PENDING_2FA_TTL_SECS=300
CHAT_LOGIN_MAX_ATTEMPTS=8
CHAT_LOGIN_WINDOW_SECS=300
CHAT_SECURE_COOKIE=true
RUST_LOG=chat_transfer=info,tower_http=info
EOF
chown root:chatxfer "$OPT/env"
chmod 640 "$OPT/env"

echo "==> 4. systemd"
cp "$REPO/deploy/chat-transfer.service" /etc/systemd/system/chat-transfer.service
systemctl daemon-reload
systemctl enable chat-transfer

echo "==> 5. Caddyfile"
TS="$(date +%Y%m%d-%H%M%S)"
cp -a /etc/caddy/Caddyfile "/etc/caddy/Caddyfile.bak.${TS}"
cat > /etc/caddy/Caddyfile <<'CADDY'
# Chat Transfer — reverse proxy with automatic HTTPS
# DNS (grey cloud / DNS-only) must point to this host for Let's Encrypt.

easyshare.pcsensor.cloud {
	encode gzip

	# Match app upload limit (CHAT_MAX_FILE_MB=100) with headroom
	request_body {
		max_size 110MB
	}

	header {
		X-Frame-Options "DENY"
		X-Content-Type-Options "nosniff"
		Referrer-Policy "no-referrer"
		-Server
	}

	# WebSocket + HTTP reverse proxy to app on localhost
	reverse_proxy 127.0.0.1:8080 {
		header_up Host {host}
		header_up X-Real-IP {remote_host}
		header_up X-Forwarded-For {remote_host}
		header_up X-Forwarded-Proto {scheme}
		# Long-lived for /ws and large uploads
		transport http {
			read_timeout 3600s
			write_timeout 3600s
		}
	}
}
CADDY

caddy validate --config /etc/caddy/Caddyfile

echo "==> 6. start services"
systemctl restart chat-transfer
sleep 2
systemctl is-active chat-transfer
systemctl reload caddy || systemctl restart caddy
sleep 1
systemctl is-active caddy

echo "==> journal chat-transfer"
journalctl -u chat-transfer -n 50 --no-pager

echo "==> local smoke"
curl -sS -o /dev/null -w "index:%{http_code}\n" http://127.0.0.1:8080/
curl -sS -o /dev/null -w "admin:%{http_code}\n" http://127.0.0.1:8080/admin
echo -n "api/config: "
curl -sS http://127.0.0.1:8080/api/config
echo

echo "==> env non-secret keys"
grep -E '^[A-Z_]+=' "$OPT/env" | cut -d= -f1 | tr '\n' ' '
echo
grep DEVICE_TRUST "$OPT/env"
grep SECURE_COOKIE "$OPT/env"
grep REGISTRATION "$OPT/env"

echo "==> layout"
ls -la "$OPT"
ls -la "$OPT/static"

echo "==> caddy status"
systemctl status caddy --no-pager -l | head -25
journalctl -u caddy -n 20 --no-pager

echo "==> done"
