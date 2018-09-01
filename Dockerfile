FROM ubuntu:latest
MAINTAINER Dolf Starreveld "dolf@starreveld.com"

ENV	DEBIAN_FRONTEND noninteractive
RUN	apt-get update && \
	apt-get install -y apt-utils && \
	apt-get install -y apt-transport-https tzdata git curl sudo vim  && \
	rm -rf /var/lib/apt/lists/*
RUN 	groupadd -r moonfire-nvr && \
	useradd moonfire-nvr --no-log-init -m -r -g moonfire-nvr && \
	echo 'moonfire-nvr ALL=(ALL) NOPASSWD: ALL' >>/etc/sudoers
ENV	HOME /home/moonfire-nvr
COPY	--chown=moonfire-nvr:moonfire-nvr . /home/moonfire-nvr/moonfire-nvr
USER	moonfire-nvr
WORKDIR /home/moonfire-nvr/moonfire-nvr
RUN 	whoami && ls -l && \
	./scripts/setup-ubuntu.sh && \
	./scripts/setup-ubuntu.sh && \
	./scripts/build.sh -B

CMD	[ "/bin/bash" ]
