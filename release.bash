#!/bin/bash
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

# Pushes a release to Docker. See guides/build.md#release-procedure.

set -o errexit
set -o pipefail
set -o xtrace

set_latest() {
    # Our images are manifest lists (for multiple architectures).
    # "docker tag" won't copy those. The technique below is adopted from here:
    # https://github.com/docker/buildx/issues/459#issuecomment-750011371
    local image="$1"
    local hashes=($(docker manifest inspect "${image}:${version}" |
                    jq --raw-output '.manifests[].digest'))
    time docker manifest rm "${image}:latest" || true
    time docker manifest create \
        "${image}:latest" \
        "${hashes[@]/#/${image}@}"
    time docker manifest push "${image}:latest"
}

build_and_push() {
    local image="$1"
    local target="$2"
    time docker buildx build \
        --push \
        --tag="${image}:${version}" \
        --target="${target}" \
        --platform=linux/amd64,linux/arm64/v8,linux/arm/v7 \
        -f docker/Dockerfile .
}

version="$(git describe --dirty)"
if [[ ! "${version}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Expected a vX.Y.Z version tag, got ${version}." >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "git status says there's extra stuff in this directory." >&2
    exit 1
fi

build_and_push scottlamb/moonfire-nvr deploy
build_and_push scottlamb/moonfire-dev dev
set_latest scottlamb/moonfire-nvr
set_latest scottlamb/moonfire-dev
