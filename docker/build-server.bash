#!/bin/bash
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

# Build the "build-server" target. See Dockerfile.

set -o errexit
set -o pipefail
set -o xtrace

mkdir /docker-build-debug/build-server
exec > >(tee -i /docker-build-debug/build-server/output) 2>&1
date
uname -a
find /cargo-cache -ls > /docker-build-debug/build-server/cargo-cache-before
find ~/target -ls > /docker-build-debug/build-server/target-before

source ~/.buildrc

# The "mode" argument to cache mounts doesn't seem to work reliably
# (as of Docker version 20.10.5, build 55c4c88, using a docker-container
# builder), thus the chmod command.
sudo chmod 777 /cargo-cache /var/lib/moonfire-nvr/target
mkdir -p /cargo-cache/{git,registry}
ln -s /cargo-cache/{git,registry} ~/.cargo

build_profile=release-lto
cd src/server
time cargo test --features=bundled-ui
time cargo build --features=bundled-ui --profile=$build_profile
find /cargo-cache -ls > /docker-build-debug/build-server/cargo-cache-after
find ~/target -ls > /docker-build-debug/build-server/target-after
sudo install -m 755 \
    ~/platform-target/$build_profile/moonfire-nvr \
    /usr/local/bin/moonfire-nvr
date
