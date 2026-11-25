#!/bin/sh
# Construit minelib-relay sur une machine Arm d'Oracle (A1) : pour elle (aarch64), et pour
# les petites machines AMD (x86_64), qui n'ont pas assez de mémoire pour le construire.
# À lancer dans le dossier du dépôt.
set -e
command -v cargo >/dev/null || {
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    . "$HOME/.cargo/env"
}
sudo apt-get update
sudo apt-get install -y build-essential pkg-config gcc-x86-64-linux-gnu
cargo build --release -p minelib-relay
rustup target add x86_64-unknown-linux-gnu
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
    cargo build --release -p minelib-relay --target x86_64-unknown-linux-gnu
cargo install iroh-relay --version 1.4.1 --features server --locked
echo "Arm    : target/release/minelib-relay"
echo "AMD    : target/x86_64-unknown-linux-gnu/release/minelib-relay"
echo "relais : $HOME/.cargo/bin/iroh-relay"
