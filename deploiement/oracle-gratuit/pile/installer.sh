#!/bin/bash
# Minelib sur Oracle Cloud (pile v329) : installe une machine de la pile, puis la tient à
# jour.
#   installer.sh installer   au premier démarrage (amorce.sh)
#   installer.sh maj         toutes les 10 minutes (minelib-maj.timer)
#
# Dans /etc/minelib/pile.env (écrit par amorce.sh) :
#   ROLE   relais   construit Minelib, relais officiel, serveur relais d'iroh, passeur
#          relais2  serveur relais d'iroh, passeur (programmes pris dans le seau)
#          passeur  passeur seul (programmes pris dans le seau)
#   SEAU   adresse du seau de la pile (lecture et écriture de ses objets)
# Dans le seau, reglages.env (le formulaire de la pile, relu à chaque fois) : NOM,
# DUCKDNS_NOMS (le premier pour « relais », le second pour « relais2 »), DUCKDNS_JETON,
# CONTACT.
#
# Ce qu'elle dépose dans le seau : aarch64.tar.gz et x86_64.tar.gz (programmes construits par
# « relais »), ligne.txt (la ligne du relais officiel), etat-<machine>.txt (où elle en est).

set -uo pipefail
export HOME=/root
export PATH=${MINELIB_ESSAI_PATH:+$MINELIB_ESSAI_PATH:}/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export DEBIAN_FRONTEND=noninteractive

ACTION=${1:-maj}
# (Les MINELIB_ESSAI_* ne servent qu'aux essais du script, hors d'une vraie machine.)
CONF=${MINELIB_ESSAI_CONF:-/etc/minelib}
E=${MINELIB_ESSAI_ETAT:-/var/lib/minelib-pile}
B=${MINELIB_ESSAI_DONNEES:-/var/lib/minelib}
O=${MINELIB_ESSAI_PROGRAMMES:-/opt/minelib}
S=${MINELIB_ESSAI_SYSTEMD:-/etc/systemd/system}
MACHINE=$(hostname -s)
REDEMARRER=0

# Une seule à la fois (une construction dure ; la suivante attend son tour).
mkdir -p "$E"
exec 9>"$E/verrou"
flock -n 9 || exit 0

# shellcheck source=/dev/null
. "$CONF/pile.env"
NOM=Minelib
DUCKDNS_NOMS=""
DUCKDNS_JETON=""
DUCKDNS_NOM=""
CONTACT=""

log() { echo "$(date -u +%FT%TZ) [$ACTION] $*"; }

seau_get() { curl -fsS --retry 3 --max-time 900 "$SEAU$1" -o "$2"; }
seau_put() { curl -fsS --retry 3 --max-time 900 -X PUT --data-binary @"$2" "$SEAU$1" -o /dev/null; }
seau_etag() {
    local e
    e=$(curl -fsSI --max-time 60 "$SEAU$1" 2>/dev/null | tr -d '\r' |
        awk 'tolower($1) == "etag:" { print $2 }')
    # (Sans HEAD, le premier octet suffit : l'en-tête y est aussi.)
    [ -n "$e" ] || e=$(curl -fsS --max-time 60 -r 0-0 -D - -o /dev/null "$SEAU$1" 2>/dev/null |
        tr -d '\r' | awk 'tolower($1) == "etag:" { print $2 }')
    echo "$e"
}

# Les réglages du seau : lus ligne à ligne, jamais exécutés (une valeur avec un caractère
# qui sortirait de son rôle est ignorée).
lire_reglages() {
    local k v
    while IFS='=' read -r k v; do
        k=${k//[[:space:]]/}
        case "$k" in
            NOM | DUCKDNS_NOMS | DUCKDNS_JETON | CONTACT) ;;
            *) continue ;;
        esac
        v=${v#\'}
        v=${v%\'}
        case "$v" in
            *[\'\"\`\$\\%]*) continue ;;
        esac
        printf -v "$k" '%s' "$v"
    done < "$1"
}

paquets() {
    apt-get -o DPkg::Lock::Timeout=900 install -y -q "$@" >/dev/null
}

# Écrit $2 dans le fichier $1 s'il a changé ; vrai s'il a changé.
ecrire() {
    if [ -f "$1" ] && [ "$(cat "$1")" = "$2" ]; then
        return 1
    fi
    printf '%s\n' "$2" > "$1"
    return 0
}

etat() {
    {
        echo "machine : $MACHINE ($ROLE), $(uname -m)"
        echo "le : $(date -u +%FT%TZ)"
        echo "état : $1"
        echo "adresse : ${IP:-?}"
        for s in minelib-relais iroh-relay minelib-passeur; do
            if [ -f "$S/$s.service" ]; then
                echo "$s : $(systemctl is-active "$s" 2>/dev/null)"
            fi
        done
        echo
        tail -n 25 /var/log/minelib.log 2>/dev/null
    } > "$E/etat.txt"
    seau_put "etat-$MACHINE.txt" "$E/etat.txt" || true
}

ip_publique() {
    local ip
    ip=$(curl -fsS --max-time 10 https://1.1.1.1/cdn-cgi/trace 2>/dev/null | sed -n 's/^ip=//p')
    [ -n "$ip" ] || ip=$(curl -fsS --max-time 10 https://api.ipify.org 2>/dev/null)
    [ -n "$ip" ] || ip=$(curl -fsS --max-time 10 https://checkip.amazonaws.com 2>/dev/null)
    echo "$ip" | tr -d '[:space:]'
}

duckdns() {
    [ -n "$DUCKDNS_JETON" ] && [ -n "$DUCKDNS_NOM" ] && [ -n "$1" ] || return 0
    local r
    r=$(curl -fsS --max-time 30 "https://www.duckdns.org/update?domains=$DUCKDNS_NOM&token=$DUCKDNS_JETON&ip=$1")
    [ "$r" = "OK" ] || log "DuckDNS refuse la mise à jour de $DUCKDNS_NOM ($r)"
}

# ───────────────────────────── Machine ─────────────────────────────

ouvre() {
    iptables -C INPUT "$@" -j ACCEPT 2>/dev/null && return 0
    iptables -I INPUT 5 "$@" -j ACCEPT 2>/dev/null || iptables -I INPUT 1 "$@" -j ACCEPT
}

# Les images Ubuntu d'Oracle refusent tout sauf SSH dans leur propre pare-feu, en plus de
# celui du réseau (que la pile ouvre aussi).
ports() {
    ouvre -p udp --dport 40000:40019
    if [ "$ROLE" = relais ]; then
        ouvre -p tcp --dport 27650
    fi
    if [ "$ROLE" != passeur ] && [ -n "$DUCKDNS_NOM" ]; then
        ouvre -p tcp --dport 80
        ouvre -p tcp --dport 443
        ouvre -p udp --dport 7842
    fi
    netfilter-persistent save >/dev/null 2>&1 || true
}

base() {
    id minelib >/dev/null 2>&1 || useradd -r -m -d "$B" -s /usr/sbin/nologin minelib
    mkdir -p "$O" "$B"
    chown minelib:minelib "$B"
    if [ ! -f "$E/paquets" ]; then
        echo iptables-persistent iptables-persistent/autosave_v4 boolean true | debconf-set-selections
        echo iptables-persistent iptables-persistent/autosave_v6 boolean true | debconf-set-selections
        apt-get -o DPkg::Lock::Timeout=900 update -q >/dev/null
        paquets curl ca-certificates iptables-persistent && touch "$E/paquets"
    fi
    ports
    ecrire "$S/minelib-maj.service" "[Unit]
Description=Mise à jour de Minelib (pile Oracle)
After=network-online.target

[Service]
Type=oneshot
ExecStart=/bin/bash $CONF/installer.sh maj
StandardOutput=append:/var/log/minelib.log
StandardError=append:/var/log/minelib.log" || true
    ecrire "$S/minelib-maj.timer" "[Unit]
Description=Mise à jour de Minelib (pile Oracle), toutes les 10 minutes

[Timer]
OnBootSec=3min
OnUnitActiveSec=10min

[Install]
WantedBy=timers.target" || true
    systemctl daemon-reload
    systemctl enable --now minelib-maj.timer >/dev/null 2>&1 || true
}

# ───────────────────────────── Programmes ─────────────────────────────

construire() {
    apt-get -o DPkg::Lock::Timeout=900 update -q >/dev/null
    paquets build-essential pkg-config gcc-x86-64-linux-gnu libc6-dev-amd64-cross binutils-x86-64-linux-gnu || return 1
    if [ ! -x /root/.cargo/bin/cargo ]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null || return 1
    fi
    rustup target add x86_64-unknown-linux-gnu >/dev/null || return 1
    rm -rf "$E/src" && mkdir -p "$E/src" && tar xzf "$E/source.tar.gz" -C "$E/src" || return 1
    export CARGO_TARGET_DIR="$E/cible"
    (cd "$E/src" && cargo build --release -q -p minelib-relay) || return 1
    (cd "$E/src" &&
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
        CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
        AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
        cargo build --release -q -p minelib-relay --target x86_64-unknown-linux-gnu) || return 1
    if [ ! -x "$E/iroh/bin/iroh-relay" ]; then
        cargo install -q iroh-relay --version 1.4.1 --features server --locked --root "$E/iroh" || return 1
    fi
    rm -rf "$E/pack" && mkdir -p "$E/pack/aarch64" "$E/pack/x86_64"
    cp "$CARGO_TARGET_DIR/release/minelib-relay" "$E/iroh/bin/iroh-relay" "$E/pack/aarch64/"
    cp "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/minelib-relay" "$E/pack/x86_64/"
    strip "$E/pack/aarch64/"* 2>/dev/null
    x86_64-linux-gnu-strip "$E/pack/x86_64/"* 2>/dev/null
    tar czf "$E/aarch64.tar.gz" -C "$E/pack/aarch64" . || return 1
    tar czf "$E/x86_64.tar.gz" -C "$E/pack/x86_64" . || return 1
    seau_put aarch64.tar.gz "$E/aarch64.tar.gz" || return 1
    seau_put x86_64.tar.gz "$E/x86_64.tar.gz" || return 1
    install -m 755 "$E/pack/aarch64/"* "$O/"
}

# « relais » : construit dès que le code du seau change.
programmes_construits() {
    local e
    e=$(seau_etag source.tar.gz)
    [ -n "$e" ] || return 0
    if [ "$e" = "$(cat "$E/source.etag" 2>/dev/null)" ] && [ -x "$O/minelib-relay" ]; then
        return 0
    fi
    seau_get source.tar.gz "$E/source.tar.gz" || return 0
    log "construction de Minelib (environ 45 minutes)"
    etat "construction de Minelib (environ 45 minutes)"
    if construire; then
        echo "$e" > "$E/source.etag"
        REDEMARRER=1
        log "construit"
    else
        log "échec de la construction"
        etat "échec de la construction"
    fi
}

# Les autres : prennent ce que « relais » a construit pour eux.
programmes_pris() {
    local a e
    a=$(uname -m)
    e=$(seau_etag "$a.tar.gz")
    [ -n "$e" ] || return 0
    if [ "$e" = "$(cat "$E/programmes.etag" 2>/dev/null)" ] && [ -x "$O/minelib-relay" ]; then
        return 0
    fi
    seau_get "$a.tar.gz" "$E/programmes.tar.gz" || return 0
    rm -rf "$E/programmes" && mkdir -p "$E/programmes"
    tar xzf "$E/programmes.tar.gz" -C "$E/programmes" || return 0
    install -m 755 "$E/programmes/"* "$O/"
    echo "$e" > "$E/programmes.etag"
    REDEMARRER=1
    log "programmes reçus"
}

# ───────────────────────────── Services ─────────────────────────────

# Écrit l'unité $1 ; la (re)lance si elle a changé ou si les programmes ont changé.
service() {
    local changed=0
    ecrire "$S/$1.service" "$2" && changed=1
    systemctl daemon-reload
    systemctl enable "$1" >/dev/null 2>&1
    if [ "$changed" = 1 ] || [ "$REDEMARRER" = 1 ] || ! systemctl is-active -q "$1"; then
        systemctl restart "$1"
    fi
}

unite() {
    # $1 description, $2 commande, $3 lignes en plus pour [Service]
    printf '%s\n' "[Unit]
Description=$1
After=network-online.target
Wants=network-online.target

[Service]
User=minelib
WorkingDirectory=$B
EnvironmentFile=-$CONF/services.env
ExecStart=$2
Restart=always
RestartSec=5
LimitNOFILE=65536
$3
[Install]
WantedBy=multi-user.target"
}

ligne_du_relais() {
    if [ "$ROLE" = relais ]; then
        local l
        for _ in $(seq 1 30); do
            l=$(journalctl -u minelib-relais --no-pager -o cat 2>/dev/null |
                sed -n 's/^Ligne pour relais_officiels.txt : //p' | tail -n 1)
            [ -n "$l" ] && break
            sleep 4
        done
        [ -n "$l" ] || return 1
        if ecrire "$E/ligne.txt" "$l" || [ ! -f "$E/ligne.envoyee" ]; then
            seau_put ligne.txt "$E/ligne.txt" && touch "$E/ligne.envoyee"
        fi
    else
        seau_get ligne.txt "$E/ligne.tmp" 2>/dev/null || return 1
        ecrire "$E/ligne.txt" "$(cat "$E/ligne.tmp")" || true
    fi
    cat "$E/ligne.txt"
}

services() {
    local iroh="" n
    for n in ${DUCKDNS_NOMS//,/ }; do
        iroh="$iroh${iroh:+,}https://$n.duckdns.org"
    done
    local env=""
    [ -z "$iroh" ] || env="MINELIB_RELAIS_IROH=$iroh"
    ecrire "$CONF/services.env" "$env" && REDEMARRER=1

    if [ "$ROLE" = relais ]; then
        service minelib-relais "$(unite "Relais Minelib" "$O/minelib-relay --dir $B/relay_data --name \"$NOM\"" "")"
    fi

    if [ "$ROLE" != passeur ] && [ -n "$DUCKDNS_NOM" ] && [ -x "$O/iroh-relay" ]; then
        local contact=${CONTACT:-minelib@$DUCKDNS_NOM.duckdns.org}
        ecrire "$CONF/iroh-relay.toml" "# Serveur relais d'iroh de Minelib (pile v329).
enable_relay = true
http_bind_addr = \"0.0.0.0:80\"
enable_quic_addr_discovery = true
enable_metrics = false

[tls]
https_bind_addr = \"0.0.0.0:443\"
quic_bind_addr = \"0.0.0.0:7842\"
cert_mode = \"LetsEncrypt\"
hostname = \"$DUCKDNS_NOM.duckdns.org\"
contact = \"$contact\"
cert_dir = \"$B/iroh-relay\"
prod_tls = true

# Personne ne prend tout : au plus 2 Mo/s envoyés par un même jeu, 4 Mo d'un coup.
[limits.client.rx]
bytes_per_second = 2000000
max_burst_bytes = 4000000" && REDEMARRER=1
        service iroh-relay "$(unite "Serveur relais iroh (Minelib)" "$O/iroh-relay --config-path $CONF/iroh-relay.toml" "AmbientCapabilities=CAP_NET_BIND_SERVICE")"
    fi

    local ligne
    if ligne=$(ligne_du_relais) && [ -n "$ligne" ] && [ -n "$IP" ]; then
        service minelib-passeur "$(unite "Passeur Minelib" "$O/minelib-relay --passeur --dir $B/passeur_data --ip $IP --port 40000 --relais \"$ligne\"" "")"
        return 0
    fi
    return 1
}

# ───────────────────────────── En route ─────────────────────────────

if [ "$ACTION" = maj ]; then
    # (Un nouveau script dans le seau, pile mise à jour : c'est lui qui continue.)
    if seau_get installer.sh "$E/installer.neuf" 2>/dev/null &&
        ! cmp -s "$E/installer.neuf" "$CONF/installer.sh"; then
        install -m 755 "$E/installer.neuf" "$CONF/installer.sh"
        exec 9>&-
        exec /bin/bash "$CONF/installer.sh" maj
    fi
fi

log "début ($ROLE)"
if seau_get reglages.env "$E/reglages.env" 2>/dev/null || [ -f "$E/reglages.env" ]; then
    lire_reglages "$E/reglages.env"
fi
case "$ROLE" in
    relais) DUCKDNS_NOM=$(echo "$DUCKDNS_NOMS" | cut -d, -f1) ;;
    relais2) DUCKDNS_NOM=$(echo "$DUCKDNS_NOMS" | cut -d, -s -f2) ;;
    *) DUCKDNS_NOM="" ;;
esac
DUCKDNS_NOM=${DUCKDNS_NOM//[^a-z0-9-]/}
base
IP=$(ip_publique)
duckdns "$IP"
if [ "$ROLE" = relais ]; then
    programmes_construits
else
    programmes_pris
fi
if [ ! -x "$O/minelib-relay" ]; then
    etat "en attente des programmes (construits par la première machine Arm, environ 45 minutes)"
    exit 0
fi
if services; then
    etat "en service"
else
    etat "en attente de la ligne du relais officiel"
fi
log "fin"
