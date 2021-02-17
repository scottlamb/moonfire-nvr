# syntax=docker/dockerfile:1.2.1
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
# SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

# See documentation here:
# https://github.com/moby/buildkit/blob/master/frontend/dockerfile/docs/syntax.md

# "dev-common" is the portion of "dev" (see below) which isn't specific to the
# target arch. It's sufficient for building the non-arch-specific webpack.
FROM --platform=$BUILDPLATFORM ubuntu:20.04 AS dev-common
LABEL maintainer="slamb@slamb.org"
ARG BUILD_UID=1000
ARG BUILD_GID=1000
ENV LC_ALL=C.UTF-8
COPY docker/dev-common.bash /
RUN /dev-common.bash
CMD	[ "/bin/bash", "--login" ]

# "dev" is a full development environment, suitable for shelling into or
# using with the VS Code container plugin.
FROM --platform=$BUILDPLATFORM dev-common AS dev
ARG BUILDARCH
ARG TARGETARCH
LABEL maintainer="slamb@slamb.org"
COPY docker/dev.bash /
RUN /dev.bash
USER moonfire-nvr:moonfire-nvr
WORKDIR /var/lib/moonfire-nvr

# Build the UI with node_modules and ui-dist outside the src dir.
FROM --platform=$BUILDPLATFORM dev-common AS build-ui
LABEL maintainer="slamb@slamb.org"
WORKDIR /var/lib/moonfire-nvr/src/ui
COPY ui /var/lib/moonfire-nvr/src/ui
RUN --mount=type=tmpfs,target=/var/lib/moonfire-nvr/src/ui/node_modules \
    npm ci && npm run build

# Build the Rust components. Note that dev.sh set up an environment variable
# in .buildrc that similarly changes the target dir path.
FROM --platform=$BUILDPLATFORM dev AS build-server
LABEL maintainer="slamb@slamb.org"
RUN --mount=type=cache,id=target,target=/var/lib/moonfire-nvr/target,sharing=locked,mode=1777 \
    --mount=type=bind,source=server,target=/var/lib/moonfire-nvr/src/server,readonly \
    bash -c 'set -o xtrace && \
             source ~/.buildrc && \
             cd src/server && \
             cargo test && \
             cargo build --release && \
             sudo install -m 755 ~/moonfire-nvr /usr/local/bin/moonfire-nvr'

# Deployment environment, now in the target platform.
FROM --platform=$TARGETPLATFORM ubuntu:20.04 AS deploy
LABEL maintainer="slamb@slamb.org"
ENV LC_ALL=C.UTF-8
RUN export DEBIAN_FRONTEND=noninteractive && \
    apt-get update && \
    apt-get install --assume-yes --no-install-recommends \
        ffmpeg \
        libncurses6 \
        libncursesw6 \
        locales \
        sudo \
        sqlite3 \
        tzdata \
        vim-nox && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/* && \
    ln -s moonfire-nvr /usr/local/bin/nvr
COPY --from=build-server /usr/local/bin/moonfire-nvr /usr/local/bin/moonfire-nvr
COPY --from=build-ui /var/lib/moonfire-nvr/src/ui/dist /usr/local/lib/moonfire-nvr/ui

# The install instructions say to use --user in the docker run commandline.
# Specify a non-root user just in case someone forgets.
USER 10000:10000
WORKDIR /var/lib/moonfire-nvr
ENTRYPOINT [ "/usr/local/bin/moonfire-nvr" ]
