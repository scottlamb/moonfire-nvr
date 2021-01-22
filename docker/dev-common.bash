#!/bin/bash
# Build the "dev" target. See Dockerfile.

set -o errexit
set -o pipefail
set -o xtrace

export DEBIAN_FRONTEND=noninteractive

packages=()

apt-get update

# Add yarn repository.
apt-get --assume-yes --no-install-recommends install curl gnupg ca-certificates
curl -sS https://dl.yarnpkg.com/debian/pubkey.gpg | apt-key add -
echo "deb https://dl.yarnpkg.com/debian/ stable main" \
    >> /etc/apt/sources.list.d/yarn.list

# Install all packages necessary for building (and some for testing/debugging).
packages+=(
    build-essential
    pkgconf
    locales
    nodejs
    sudo
    sqlite3
    tzdata
    vim-nox
    yarn
)
apt-get update
apt-get install --assume-yes --no-install-recommends "${packages[@]}"
apt-get clean
rm -rf /var/lib/apt/lists/*

# Create the user. On the dev environment, allow sudo.
groupadd \
    --gid="${BUILD_GID}" \
    moonfire-nvr
useradd \
    --no-log-init \
    --home-dir=/var/lib/moonfire-nvr \
    --uid="${BUILD_UID}" \
    --gid=moonfire-nvr \
    --shell=/bin/bash \
    --create-home \
    moonfire-nvr
echo 'moonfire-nvr ALL=(ALL) NOPASSWD: ALL' >>/etc/sudoers

# Install Rust. Note curl was already installed for yarn above.
su moonfire-nvr -lc "curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs |
                     sh -s - -y"

# Put configuration for the Rust build into a new ".buildrc" which is used
# both (1) interactively from ~/.bashrc when logging into the dev container
# and (2) from a build-server RUN command. In particular, the latter can't
# use ~/.bashrc because that script immediately exits when run from a
# non-interactive shell.
echo 'source $HOME/.buildrc' >> /var/lib/moonfire-nvr/.bashrc
cat >> /var/lib/moonfire-nvr/.buildrc <<EOF
source \$HOME/.cargo/env

# Set the target directory to be outside the src bind mount.
# https://doc.rust-lang.org/cargo/reference/config.html#buildtarget-dir
export CARGO_BUILD_TARGET_DIR=/var/lib/moonfire-nvr/target
EOF
chown moonfire-nvr:moonfire-nvr /var/lib/moonfire-nvr/.buildrc
