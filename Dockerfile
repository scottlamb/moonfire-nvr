FROM ubuntu:latest
MAINTAINER Dolf Starreveld "dolf@starreveld.com"

ENV	DEBIAN_FRONTEND noninteractive
RUN	apt-get update && \
	apt-get install -y apt-utils && \
	apt-get install -y apt-transport-https tzdata git curl sudo vim  && \
	rm -rf /var/lib/apt/lists/*
RUN	useradd --no-log-init --home-dir /var/lib/moonfire-nvr --system --user-group moonfire-nvr && \
	echo 'moonfire-nvr ALL=(ALL) NOPASSWD: ALL' >>/etc/sudoers
ENV	HOME /var/lib/moonfire-nvr
COPY	--chown=moonfire-nvr:moonfire-nvr . /home/moonfire-nvr/moonfire-nvr
USER	moonfire-nvr
WORKDIR /var/lib/moonfire-nvr/moonfire-nvr
RUN 	whoami && ls -l && \
	./scripts/setup-ubuntu.sh && \
	./scripts/setup-ubuntu.sh && \
	./scripts/build.sh -B

CMD	[ "/bin/bash" ]
