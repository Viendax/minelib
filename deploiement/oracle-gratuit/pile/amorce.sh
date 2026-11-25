#!/bin/bash
# Minelib (v329) : premier démarrage d'une machine de la pile. Garde son rôle et l'adresse du
# seau, prend le script d'installation dans le seau et le lance ; il se relance ensuite tout
# seul (minelib-maj.timer) et relit les réglages du seau (reglages.env) à chaque fois.
# Modèle rempli par main.tf : seul ce qui est entre accolades change.
mkdir -p /etc/minelib
umask 077
cat > /etc/minelib/pile.env <<'FIN'
ROLE='${role}'
SEAU='${seau}'
FIN
umask 022
until curl -fsS --max-time 60 '${seau}installer.sh' -o /etc/minelib/installer.sh; do
    sleep 20
done
bash /etc/minelib/installer.sh installer >> /var/log/minelib.log 2>&1
