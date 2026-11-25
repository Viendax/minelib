#!/bin/sh
# Ouvre les ports de Minelib dans le pare-feu de la machine (les images Ubuntu d'Oracle
# refusent tout sauf SSH). À lancer avec sudo, une fois. `relais` en argument sur une machine
# qui fait tourner iroh-relay.
set -e
# Passeurs : un port UDP par relais servi, à partir de 40000.
iptables -I INPUT 5 -p udp --dport 40000:40019 -j ACCEPT
if [ "$1" = "relais" ]; then
    iptables -I INPUT 5 -p tcp --dport 80 -j ACCEPT
    iptables -I INPUT 5 -p tcp --dport 443 -j ACCEPT
    iptables -I INPUT 5 -p udp --dport 7842 -j ACCEPT
fi
apt-get install -y iptables-persistent >/dev/null 2>&1 || true
netfilter-persistent save
echo "ports ouverts"
