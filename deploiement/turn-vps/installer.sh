#!/bin/sh
# Minelib (v334) : installe coturn depuis les paquets de la distribution (Debian, Ubuntu),
# sans Docker, avec turnserver.conf. À lancer en root, depuis ce dossier :
#   sh installer.sh
# À la fin : le secret à donner au Worker (COTURN_SECRET). Relancer garde le même secret.
set -e
cd "$(dirname "$0")"
apt-get update
apt-get install -y coturn openssl
ANCIEN=$(sed -n 's/^static-auth-secret=//p' /etc/turnserver.conf 2>/dev/null || true)
if [ -n "$ANCIEN" ] && [ "$ANCIEN" != "A_REMPLACER" ]; then
  SECRET="$ANCIEN"
else
  SECRET=$(openssl rand -hex 24)
fi
sed "s/^static-auth-secret=.*/static-auth-secret=$SECRET/" turnserver.conf > /etc/turnserver.conf
chown root:turnserver /etc/turnserver.conf 2>/dev/null || true
chmod 640 /etc/turnserver.conf
# (Anciennes versions d'Ubuntu : le service ne démarre qu'avec cette ligne.)
if [ -f /etc/default/coturn ]; then
  sed -i 's/^#\?TURNSERVER_ENABLED=.*/TURNSERVER_ENABLED=1/' /etc/default/coturn
fi
systemctl enable coturn
systemctl restart coturn
if command -v ufw >/dev/null 2>&1 && ufw status | grep -q "Status: active"; then
  ufw allow 3478/udp
  ufw allow 3478/tcp
  ufw allow 49152:65535/udp
fi
IP=$(curl -fsS https://api.ipify.org 2>/dev/null || hostname -I | cut -d' ' -f1)
echo
echo "coturn tourne. Pour le Worker (Settings -> Variables and Secrets, type Secret) :"
echo "  COTURN_URLS   = turn:$IP:3478"
echo "  COTURN_SECRET = $SECRET"
echo
echo "Essai depuis le PC (PowerShell, dossier du dépôt), avec des identifiants d'un jour :"
U="$(( $(date +%s) + 86400 )):essai"
P=$(printf %s "$U" | openssl dgst -sha1 -hmac "$SECRET" -binary | openssl base64)
echo "  \$env:MINELIB_TURN_ESSAI=\"turn:$IP:3478 $U $P\"; cargo test -p minelib-p2p --lib real_turn -- --ignored --nocapture"
