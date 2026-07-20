#!/bin/bash
set -euo pipefail

REPO=/home/justin/pcsensor/web-share
OPT=/opt/chat-transfer
TS=$(date +%Y%m%d-%H%M%S)

echo "==> backup $TS"
mkdir -p /root/chat-transfer-backups
cp -a "$OPT/chat-transfer" "/root/chat-transfer-backups/chat-transfer.$TS" || true
cp -a "$OPT/env" "/root/chat-transfer-backups/env.$TS" || true
tar czf "/root/chat-transfer-backups/data.$TS.tgz" -C "$OPT" data

echo "==> build production env from repo .env"
python3 - <<'PY'
from pathlib import Path

src = Path("/home/justin/pcsensor/web-share/.env").read_text(encoding="utf-8")
overrides = {
    "CHAT_BIND": "127.0.0.1:8080",
    "CHAT_DATA_DIR": "/opt/chat-transfer/data",
    "CHAT_SECURE_COOKIE": "false",
}
out_lines = []
seen = set()
for line in src.splitlines():
    s = line.strip()
    if not s or s.startswith("#") or "=" not in s:
        out_lines.append(line)
        continue
    k, _v = s.split("=", 1)
    k = k.strip()
    if k in overrides:
        out_lines.append(f"{k}={overrides[k]}")
        seen.add(k)
    else:
        out_lines.append(line)
        seen.add(k)
for k, v in overrides.items():
    if k not in seen:
        out_lines.append(f"{k}={v}")
text = "\n".join(out_lines) + "\n"
if "CHAT_PENDING_2FA_TTL_SECS" not in text:
    text += "CHAT_PENDING_2FA_TTL_SECS=300\n"
Path("/tmp/chat-transfer.env.new").write_text(text, encoding="utf-8")
keys = [l.split("=", 1)[0] for l in text.splitlines() if l and not l.startswith("#") and "=" in l]
print("env keys:", ", ".join(keys))
PY

echo "==> stop service"
systemctl stop chat-transfer

echo "==> install binary + static"
install -o chatxfer -g chatxfer -m 550 \
  "$REPO/target/release/chat-transfer" "$OPT/chat-transfer"
rsync -a --delete "$REPO/static/" "$OPT/static/"
chown -R chatxfer:chatxfer "$OPT/static" "$OPT/data"
install -o root -g chatxfer -m 640 /tmp/chat-transfer.env.new "$OPT/env"
rm -f /tmp/chat-transfer.env.new

echo "==> refresh systemd unit"
cp "$REPO/deploy/chat-transfer.service" /etc/systemd/system/chat-transfer.service
systemctl daemon-reload

echo "==> start service"
systemctl start chat-transfer
sleep 2
systemctl is-active chat-transfer
echo "==> journal"
journalctl -u chat-transfer -n 30 --no-pager

echo "==> local smoke"
curl -sS -o /dev/null -w "index:%{http_code}\n" http://127.0.0.1:8080/
curl -sS -o /dev/null -w "admin:%{http_code}\n" http://127.0.0.1:8080/admin
curl -sS http://127.0.0.1:8080/api/config
echo
echo "==> done"
