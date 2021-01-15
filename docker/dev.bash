#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace

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

    dpkg --add-architecture "${dpkg_arch}"

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
        g++-${gcc_target}
        libc6-dev-${dpkg_arch}-cross
        pkg-config-${gcc_target}
        qemu-user
    )
fi

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
    ffmpeg"${apt_target_suffix}"
    libavcodec-dev"${apt_target_suffix}"
    libavformat-dev"${apt_target_suffix}"
    libavutil-dev"${apt_target_suffix}"
    libncurses-dev"${apt_target_suffix}"
    libsqlite3-dev"${apt_target_suffix}"
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

# Set environment variables for cross-compiling.
# Also set up a symlink that points to the release binary, because the
# release binary's location varies when cross-compiling, as described here:
# https://doc.rust-lang.org/cargo/guide/build-cache.html
if [[ -n "${rust_target}" ]]; then
    su moonfire-nvr -lc "rustup target install ${rust_target} &&
                         ln -s target/${rust_target}/release/moonfire-nvr ."
    underscore_rust_target="${rust_target//-/_}"
    uppercase_underscore_rust_target="${underscore_rust_target^^}"
    cat >> /var/lib/moonfire-nvr/.buildrc <<EOF

# https://doc.rust-lang.org/cargo/reference/config.html
export CARGO_BUILD_TARGET=${rust_target}
export CARGO_TARGET_${uppercase_underscore_rust_target}_LINKER=${gcc_target}-gcc

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
    su moonfire-nvr -lc "ln -s target/release/moonfire-nvr ."
fi
