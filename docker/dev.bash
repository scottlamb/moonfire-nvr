#!/bin/bash
# Build the "dev" target. See Dockerfile.

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

# Install the packages for the target architecture.
packages+=(
    ffmpeg"${apt_target_suffix}"
    libavcodec-dev"${apt_target_suffix}"
    libavformat-dev"${apt_target_suffix}"
    libavutil-dev"${apt_target_suffix}"
    libncurses-dev"${apt_target_suffix}"
    libsqlite3-dev"${apt_target_suffix}"
)
apt-get update
apt-get install --assume-yes --no-install-recommends "${packages[@]}"
apt-get clean
rm -rf /var/lib/apt/lists/*

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
