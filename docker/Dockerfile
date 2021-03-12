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
ARG INVALIDATE_CACHE_DEV_COMMON=
ENV LC_ALL=C.UTF-8
COPY docker/dev-common.bash /
RUN --mount=type=cache,id=var-cache-apt,target=/var/cache/apt,sharing=locked \
    /dev-common.bash
CMD [ "/bin/bash", "--login" ]

# "dev" is a full development environment, suitable for shelling into or
# using with the VS Code container plugin.
FROM --platform=$BUILDPLATFORM dev-common AS dev
LABEL maintainer="slamb@slamb.org"
ARG BUILDARCH
ARG TARGETARCH
ARG INVALIDATE_CACHE_DEV=
COPY docker/dev.bash /
RUN --mount=type=cache,id=var-cache-apt,target=/var/cache/apt,sharing=locked \
    /dev.bash
USER moonfire-nvr:moonfire-nvr
WORKDIR /var/lib/moonfire-nvr

# Build the UI with node_modules and ui-dist outside the src dir.
FROM --platform=$BUILDPLATFORM dev-common AS build-ui
ARG INVALIDATE_CACHE_BUILD_UI=
LABEL maintainer="slamb@slamb.org"
WORKDIR /var/lib/moonfire-nvr/src/ui
COPY docker/build-ui.bash /
COPY ui /var/lib/moonfire-nvr/src/ui
RUN --mount=type=tmpfs,target=/var/lib/moonfire-nvr/src/ui/node_modules \
    /build-ui.bash

# Build the Rust components. Note that dev.sh set up an environment variable
# in .buildrc that similarly changes the target dir path.
FROM --platform=$BUILDPLATFORM dev AS build-server
LABEL maintainer="slamb@slamb.org"
ARG INVALIDATE_CACHE_BUILD_SERVER=
COPY docker/build-server.bash /
RUN --mount=type=cache,id=target,target=/var/lib/moonfire-nvr/target,sharing=locked,mode=777 \
    --mount=type=cache,id=cargo,target=/cargo-cache,sharing=locked,mode=777 \
    --mount=type=bind,source=server,target=/var/lib/moonfire-nvr/src/server,readonly \
    /build-server.bash

# Deployment environment, now in the target platform.
FROM --platform=$TARGETPLATFORM ubuntu:20.04 AS deploy
LABEL maintainer="slamb@slamb.org"
ARG INVALIDATE_CACHE_BUILD_DEPLOY=
ENV LC_ALL=C.UTF-8
COPY docker/deploy.bash /
RUN --mount=type=cache,id=var-cache-apt,target=/var/cache/apt,sharing=locked \
    /deploy.bash
COPY --from=dev-common /docker-build-debug/dev-common/ /docker-build-debug/dev-common/
COPY --from=dev /docker-build-debug/dev/ /docker-build-debug/dev/
COPY --from=build-server /docker-build-debug/build-server/ /docker-build-debug/build-server/
COPY --from=build-server /usr/local/bin/moonfire-nvr /usr/local/bin/moonfire-nvr
COPY --from=build-ui /docker-build-debug/build-ui /docker-build-debug/build-ui
COPY --from=build-ui /var/lib/moonfire-nvr/src/ui/build /usr/local/lib/moonfire-nvr/ui

# The install instructions say to use --user in the docker run commandline.
# Specify a non-root user just in case someone forgets.
USER 10000:10000
WORKDIR /var/lib/moonfire-nvr
ENTRYPOINT [ "/usr/local/bin/moonfire-nvr" ]
