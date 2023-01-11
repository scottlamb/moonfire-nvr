#!/bin/bash
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

# Build the "dev" target. See Dockerfile.

set -o errexit
set -o pipefail
set -o xtrace

mkdir /docker-build-debug/dev
exec > >(tee -i /docker-build-debug/dev/output) 2>&1

date
uname -a
find /var/cache/apt -ls > /docker-build-debug/dev/var-cache-apt-before

export DEBIAN_FRONTEND=noninteractive

packages=()

if [[ "${BUILDARCH}" != "${TARGETARCH}" ]]; then
    # Set up cross compilation.
    case "${TARGETARCH}" in
    arm64)
      dpkg_arch=arm64
      gcc_target=aarch64-linux-gnu
      rust_target=aarch64-unknown-linux-gnu
      target_is_port=1
      ;;
    arm)
      dpkg_arch=armhf
      gcc_target=arm-linux-gnueabihf
      rust_target=arm-unknown-linux-gnueabihf
      target_is_port=1
      ;;
    amd64)
      dpkg_arch=amd64
      gcc_target=x86_64-linux-gnu
      rust_target=x86_64-unknown-linux-gnu
      target_is_port=0
      ;;
    *)
      echo "Unsupported cross-compile target ${TARGETARCH}." >&2
      exit 1
    esac
    apt_target_suffix=":${dpkg_arch}"
    case "${BUILDARCH}" in
    amd64|386) host_is_port=0 ;;
    *) host_is_port=1 ;;
    esac

    time dpkg --add-architecture "${dpkg_arch}"

    if [[ $target_is_port -ne $host_is_port ]]; then
        # Ubuntu stores non-x86 architectures at a different URL, so futz the
        # sources file to allow installing both host and target.
        # See https://github.com/rust-embedded/cross/blob/master/docker/common.sh
        perl -pi.bak -e '
            s{^deb (http://.*.ubuntu.com/ubuntu/) (.*)}
             {deb [arch=amd64,i386] \1 \2\ndeb [arch-=amd64,i386] http://ports.ubuntu.com/ubuntu-ports \2};
            s{^deb (http://ports.ubuntu.com/ubuntu-ports/) (.*)}
             {deb [arch=amd64,i386] http://archive.ubuntu.com/ubuntu \2\ndeb [arch-=amd64,i386] \1 \2}' \
            /etc/apt/sources.list
        cat /etc/apt/sources.list
    fi

    packages+=(
        g++-${gcc_target/_/-}
        libc6-dev-${dpkg_arch}-cross
        qemu-user
    )
fi

time apt-get update

# Install the packages for the target architecture.
packages+=(
    libncurses-dev"${apt_target_suffix}"
    libsqlite3-dev"${apt_target_suffix}"
)
time apt-get update
time apt-get install --assume-yes --no-install-recommends "${packages[@]}"

# Set environment variables for cross-compiling.
# Also set up a symlink that points to the output platform's target dir, because
# the target directory layout varies when cross-compiling, as described here:
# https://doc.rust-lang.org/cargo/guide/build-cache.html
if [[ -n "${rust_target}" ]]; then
    su moonfire-nvr -lc "rustup target install ${rust_target} &&
                         ln -s target/${rust_target} platform-target"
    underscore_rust_target="${rust_target//-/_}"
    uppercase_underscore_rust_target="${underscore_rust_target^^}"
    cat >> /var/lib/moonfire-nvr/.buildrc <<EOF

# https://doc.rust-lang.org/cargo/reference/config.html
export CARGO_BUILD_TARGET=${rust_target}
export CARGO_TARGET_${uppercase_underscore_rust_target}_LINKER=${gcc_target}-gcc

# Use a pkg-config wrapper for the target architecture. This wrapper was
# automatically created on "dpkg --add-architecture"; see
# /etc/dpkg/dpkg.cfg.d/pkgconf-hook-config.
#
# https://github.com/rust-lang/pkg-config-rs uses the "PKG_CONFIG"
# variable to to select the pkg-config binary to use. As of pkg-config 0.3.19,
# it unfortunately doesn't understand the <target>_ prefix that the README.md
# describes for other vars. Fortunately Moonfire NVR doesn't have any host tools
# that need pkg-config.
export PKG_CONFIG=${gcc_target}-pkg-config

# https://github.com/alexcrichton/cc-rs uses these variables to decide what
# compiler to invoke.
export CC_${underscore_rust_target}=${gcc_target}-gcc
export CXX_${underscore_rust_target}=${gcc_target}-g++
EOF
else
    su moonfire-nvr -lc "ln -s target platform-target"
fi

find /var/cache/apt -ls > /docker-build-debug/dev/var-cache-apt-after
date
