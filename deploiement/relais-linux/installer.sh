#!/bin/sh
# Minelib (v342) : le relais Minelib sur une machine Linux (x86_64, systemd), qui se met à
# jour tout seul à chaque nouvelle version publiée sur GitHub (celle des joueurs).
#
#   sudo sh installer.sh relais  [options de minelib-relay : --name <nom>, --journaux,
#                                --admin <pseudo>, --place-mondes <Mo>…]
#   sudo sh installer.sh passeur [--ip <adresse publique>] [--port <premier port UDP>]
#                                [--relais "<nœud> <clé> <nom>"] [--invitation "<…>"]
#                                [--sans-journaux] [--place-cartes <Mo>]
#
# (v345) Les deux sur une même machine : chacun a son service (minelib-relay, minelib-passeur).
# `--invitation` : celle donnée dans le jeu (/forwarder invite) ; le passeur y devient de
# confiance et porte aussi les parties FPS. Rien à refaire ensuite. `--admin <pseudo>` : ce
# compte administre le relais (bannir pour triche).
#
# Ce qu'il met en place :
# - un compte système `minelib`, sans connexion possible ;
# - le programme dans /opt/minelib-relay, ses données dans /var/lib/minelib-relay ;
# - le service `minelib-relay` (relancé s'il s'arrête, et au démarrage de la machine) ;
# - la minuterie `minelib-relay-maj` : toutes les dix minutes, une version plus récente ?
#   Téléchargée depuis la Release de GitHub, empreinte SHA-256 vérifiée, essayée sur cette
#   machine, puis mise en place et le service relancé. Rien à compiler ici.
#
# Relancer ce script change les options du service (sans rien perdre des données).
set -eu

DIR=/opt/minelib-relay
DATA=/var/lib/minelib-relay

[ "$(id -u)" = 0 ] || { echo "À lancer en root : sudo sh installer.sh relais|passeur …"; exit 1; }
[ "$(uname -m)" = x86_64 ] || { echo "Seulement pour x86_64 (cette machine : $(uname -m))."; exit 1; }
command -v systemctl >/dev/null 2>&1 || { echo "Il faut systemd."; exit 1; }
if ! command -v curl >/dev/null 2>&1; then
    if command -v apt-get >/dev/null 2>&1; then
        apt-get install -y -q curl ca-certificates >/dev/null
    else
        echo "Il faut curl."
        exit 1
    fi
fi

mode=${1:-}
case "$mode" in
    relais)
        shift
        set -- --dir "$DATA/relay_data" "$@"
        unit=minelib-relay
        ;;
    passeur)
        shift
        set -- --passeur --dir "$DATA/passeur_data" "$@"
        unit=minelib-passeur
        ;;
    *)
        echo "Usage : sudo sh installer.sh relais|passeur [options]"
        exit 2
        ;;
esac

# Une option par argument, entre guillemets (systemd : \ et " protégés, % et $ doublés).
q() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"]/\\&/g' -e 's/%/%%/g' -e 's/\$/$$/g')"
}
cmd="$DIR/minelib-relay"
for a in "$@"; do
    cmd="$cmd $(q "$a")"
done

id minelib >/dev/null 2>&1 || useradd --system --home-dir "$DATA" --shell /usr/sbin/nologin minelib
mkdir -p "$DIR" "$DATA"
chown minelib:minelib "$DATA"

cat > "$DIR/maj.sh" <<'MAJ'
#!/bin/sh
# Minelib (v342) : met le relais à la dernière version publiée sur GitHub, s'il y en a une
# plus récente (minuterie minelib-relay-maj, toutes les dix minutes).
set -eu
REPO=Viendax/minelib
DIR=/opt/minelib-relay
# La dernière version : la page « latest » renvoie vers …/tag/vNN.
tag=$(curl -fsSI --max-time 30 "https://github.com/$REPO/releases/latest" | tr -d '\r' |
    sed -n 's|^[Ll]ocation: .*/tag/v\([0-9][0-9]*\)$|\1|p' | tail -n 1)
[ -n "$tag" ] || exit 0
cur=$(cat "$DIR/version" 2>/dev/null || echo 0)
[ "$tag" -gt "$cur" ] || exit 0
name="minelib-relay_v${tag}_linux-x86_64"
url="https://github.com/$REPO/releases/download/v$tag/$name"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Pas encore là (la compilation pour Linux suit celle pour Windows) : à la prochaine fois.
curl -fsSL --max-time 900 -o "$tmp/bin" "$url" || exit 0
curl -fsSL --max-time 60 -o "$tmp/sum" "$url.sha256" || exit 0
want=$(tr -d ' \r\n' < "$tmp/sum")
got=$(sha256sum "$tmp/bin" | cut -d' ' -f1)
if [ "$want" != "$got" ]; then
    echo "v$tag : empreinte fausse, rien n'est changé" >&2
    exit 1
fi
chmod 755 "$tmp/bin"
# Démarre-t-il sur cette machine (bibliothèque C assez récente) ? Une option inconnue le
# fait s'arrêter tout de suite, avec le code 2.
set +e
"$tmp/bin" --essai-de-lancement >/dev/null 2>&1
rc=$?
set -e
if [ "$rc" != 2 ]; then
    echo "v$tag : ne démarre pas sur cette machine (code $rc), rien n'est changé" >&2
    exit 1
fi
cp "$tmp/bin" "$DIR/minelib-relay.new"
mv -f "$DIR/minelib-relay.new" "$DIR/minelib-relay"
echo "$tag" > "$DIR/version"
# (v345) Le relais et le passeur de cette machine, ceux qui y sont installés.
for u in minelib-relay minelib-passeur; do
    if [ -f "/etc/systemd/system/$u.service" ]; then
        systemctl restart "$u"
    fi
done
echo "relais Minelib : v$tag"
MAJ
chmod 755 "$DIR/maj.sh"

# (v345) Avant, un passeur s'installait sous le nom du relais : il prend le sien.
if [ "$unit" = minelib-passeur ] && grep -q -- '--passeur' /etc/systemd/system/minelib-relay.service 2>/dev/null; then
    systemctl disable -q --now minelib-relay 2>/dev/null || true
    rm -f /etc/systemd/system/minelib-relay.service
fi

cat > /etc/systemd/system/$unit.service <<UNIT
[Unit]
Description=Minelib : $mode
After=network-online.target
Wants=network-online.target
# (Pas encore téléchargé : la minuterie le démarre quand il est là.)
ConditionPathExists=$DIR/minelib-relay

[Service]
User=minelib
WorkingDirectory=$DATA
ExecStart=$cmd
Restart=always
RestartSec=5
LimitNOFILE=65536
NoNewPrivileges=true
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
UNIT

cat > /etc/systemd/system/minelib-relay-maj.service <<UNIT
[Unit]
Description=Minelib : mise à jour du relais
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$DIR/maj.sh
UNIT

cat > /etc/systemd/system/minelib-relay-maj.timer <<UNIT
[Unit]
Description=Minelib : mise à jour du relais, toutes les dix minutes

[Timer]
OnBootSec=1min
OnUnitActiveSec=10min
RandomizedDelaySec=60

[Install]
WantedBy=timers.target
UNIT

systemctl daemon-reload
systemctl enable -q "$unit" minelib-relay-maj.timer
systemctl start minelib-relay-maj.timer
"$DIR/maj.sh" || true
if [ -x "$DIR/minelib-relay" ]; then
    systemctl restart "$unit"
    echo "Minelib v$(cat "$DIR/version") en route ($mode)."
else
    echo "Pas encore de version pour Linux sur GitHub : la minuterie l'installera dès qu'elle"
    echo "y sera (dans les dix minutes qui suivent sa publication)."
fi
echo "État : systemctl status $unit ; journal : journalctl -u $unit -f"
