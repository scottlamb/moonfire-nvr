#!/bin/bash
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

# Build the "build-ui" target. See Dockerfile.

set -o errexit
set -o pipefail
set -o xtrace

mkdir /docker-build-debug/build-ui
exec > >(tee -i /docker-build-debug/build-ui/output) 2>&1

date
uname -a
node --version
npm --version
time npm ci
time npm run build

ls -laFR /var/lib/moonfire-nvr/src/ui/node_modules \
    > /docker-build-debug/build-ui/node_modules-after
date
