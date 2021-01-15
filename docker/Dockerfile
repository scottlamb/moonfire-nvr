# syntax=docker/dockerfile:1.2.1

# See documentation here:
# https://github.com/moby/buildkit/blob/master/frontend/dockerfile/docs/syntax.md

# Moonfire NVR development environment, using the build platform.
FROM --platform=$BUILDPLATFORM ubuntu:20.04 AS dev
LABEL maintainer="slamb@slamb.org"
ARG TARGETARCH
ARG BUILDARCH
ARG BUILD_UID=1000
ARG BUILD_GID=1000
LABEL maintainer="slamb@slamb.org"
ENV LC_ALL=C.UTF-8
COPY docker/dev.bash /
RUN /dev.bash
USER moonfire-nvr:moonfire-nvr
WORKDIR /var/lib/moonfire-nvr
CMD	[ "/bin/bash", "--login" ]

# Build the webpack with node_modules and ui-dist outside the src dir.
FROM dev AS build-webpack
LABEL maintainer="slamb@slamb.org"
RUN --mount=type=bind,target=/var/lib/moonfire-nvr/src,readonly \
    ln -s src/package.json src/yarn.lock src/ui-src src/webpack . && \
    yarn install && yarn build && rm -rf node_modules

# Build the Rust components. Note that dev.sh set up an environment variable
# in .buildrc that similarly changes the target dir path.
FROM dev AS build-server
LABEL maintainer="slamb@slamb.org"
RUN --mount=type=bind,target=/var/lib/moonfire-nvr/src,readonly \
    bash -c 'set -o xtrace && source ~/.buildrc && cd src && cargo test && cargo build --release'

# Deployment environment, now in the target platform.
FROM --platform=$TARGETPLATFORM ubuntu:20.04 AS deploy
ARG DEPLOY_UID=10000
ARG DEPLOY_GID=10000
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
    groupadd \
        --gid="${DEPLOY_GID}" \
        moonfire-nvr && \
    useradd \
        --no-log-init \
        --home-dir=/var/lib/moonfire-nvr \
        --uid="${DEPLOY_UID}" \
        --gid=moonfire-nvr \
        --shell=/bin/bash \
        --create-home \
        moonfire-nvr && \
    ln -s moonfire-nvr /usr/local/bin/nvr
COPY --from=build-server /var/lib/moonfire-nvr/moonfire-nvr /usr/local/bin/moonfire-nvr
COPY --from=build-webpack /var/lib/moonfire-nvr/ui-dist/* /usr/local/moonfire-nvr/ui

USER moonfire-nvr:moonfire-nvr
WORKDIR /var/lib/moonfire-nvr
ENTRYPOINT [ "/usr/local/bin/moonfire-nvr" ]
