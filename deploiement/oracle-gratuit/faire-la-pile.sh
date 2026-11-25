#!/bin/sh
# Minelib (v329) : fait target/minelib-oracle.zip, la pile à donner à Resource Manager
# d'Oracle Cloud (voir LISEZMOI.md), avec le code de la dernière version enregistrée
# (git HEAD). À lancer dans le dépôt (sous Windows : dans Git Bash).
set -e
cd "$(git rev-parse --show-toplevel)"
out=target/pile-oracle
rm -rf "$out" target/minelib-oracle.zip
mkdir -p "$out/src"

# Le code du relais seulement (pas le jeu), dans un espace de travail réduit à lui.
git archive HEAD Cargo.lock relais_officiels.txt relais_iroh.txt relais_turn.txt \
    crates/net crates/p2p crates/relay | tar -x -C "$out/src"
sed -e 's|^members = .*|members = ["crates/net", "crates/p2p", "crates/relay"]|' \
    -e '/^exclude = /d' Cargo.toml > "$out/src/Cargo.toml"
tar -czf "$out/source.tar.gz" -C "$out/src" .
rm -rf "$out/src"

cp deploiement/oracle-gratuit/pile/main.tf deploiement/oracle-gratuit/pile/schema.yaml \
    deploiement/oracle-gratuit/pile/amorce.sh deploiement/oracle-gratuit/pile/installer.sh "$out/"

cd "$out"
if command -v zip >/dev/null 2>&1; then
    zip -q -X ../minelib-oracle.zip main.tf schema.yaml amorce.sh installer.sh source.tar.gz
elif [ -x /c/Windows/System32/tar.exe ]; then
    # (Git Bash n'a pas zip ; le tar de Windows en fait.)
    /c/Windows/System32/tar.exe -a -cf ../minelib-oracle.zip main.tf schema.yaml amorce.sh installer.sh source.tar.gz
else
    python3 -c 'import sys, zipfile
z = zipfile.ZipFile("../minelib-oracle.zip", "w", zipfile.ZIP_DEFLATED)
for f in sys.argv[1:]:
    z.write(f)' main.tf schema.yaml amorce.sh installer.sh source.tar.gz
fi
cd ..
echo "Pile prête : $(pwd)/minelib-oracle.zip"
