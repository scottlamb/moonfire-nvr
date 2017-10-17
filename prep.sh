#!/bin/bash -x
#
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# In addition, as a special exception, the copyright holders give
# permission to link the code of portions of this program with the
# OpenSSL library under certain conditions as described in each
# individual source file, and distribute linked combinations including
# the two.
#
# You must obey the GNU General Public License in all respects for all
# of the code used other than OpenSSL. If you modify file(s) with this
# exception, you may extend this exception to your version of the
# file(s), but you are not obligated to do so. If you do not wish to do
# so, delete this exception statement from your version. If you delete
# this exception statement from all source files in the program, then
# also delete it here.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.
#
#
# Script to prepare for moonfire-nvr operations
#
# Command line options:
# -S: Skip apt-get update and install
#

# Configuration variables. Should only need minimal, or zero, changes.
# Empty values will use defaults. For convenience, we attempt to read
# customizations from prep.config first
#
if [ -r prep.config ]; then
	. prep.config
fi

# User and group
# Default: or whatever is in $NVR_USER (default "moonfire-nvr")
#
#NVR_USER=
#NVR_GROUP=

# Port for web server
# Default: 8080
#
#NVR_PORT=

# This should, ideally, be a location on flash storage under which the
# moonfire user's home directory will be created.
# Default: "/var/lib"
#
#NVR_HOME_BASE=

# Set to mountpoint of media directory, empty to stay in home directory
# Default: empty
#SAMPLES_DIR=/media/nvr/samples

# Set to path for media directory relative to mountpoint
# Default: "samples"
#
#SAMPLES_DIR_NAME=

# Binary location
# Default: "/usr/local/bin/moonfire-nvr"
#
#SERVICE_BIN=

# Service name
# Default: "moonfire-nvr"
#
#SERVICE_NAME=

# Service Description
# Default: "Moonfire NVR"
#
#SERVICE_DESC=

# --------------------------------------------------------------------
# Derived variables: Do not modify!
#
# Determine directory path of this script
#
SOURCE="${BASH_SOURCE[0]}"
while [ -h "$SOURCE" ]; do # resolve $SOURCE until file no longer a symlink
	DIR="$( cd -P "$( dirname "$SOURCE" )" && pwd )"
	SOURCE="$(readlink "$SOURCE")"
	# if $SOURCE was relative symlink, resolve relative to path of symlink
	[[ "$SOURCE" != /* ]] && SOURCE="$DIR/$SOURCE"
done

SRC_DIR="$( cd -P "$( dirname "$SOURCE" )" && pwd )"/src
NVR_USER="${NVR_USER:-moonfire-nvr}"
NVR_GROUP="${NVR_GROUP:-$NVR_USER}"
NVR_PORT="${NVR_PORT:-8080}"
NVR_HOME_BASE="${NVR_HOME_BASE:-/var/lib}"
NVR_HOME="${NVR_HOME_BASE}/${NVR_USER}"
DB_DIR="${DB_DIR:-$NVR_HOME/db}"
SAMPLES_DIR_NAME="${SAMPLES_DIR_NAME:-samples}"
SAMPLES_DIR="${SAMPLES_DIR:-$NVR_HOME/$SAMPLES_DIR_NAME}"
SERVICE_NAME="${SERVICE_NAME:-moonfire-nvr}"
SERVICE_DESC="${SERVICE_DESC:-Moonfire NVR}"
SERVICE_BIN="${SERVICE_BIN:-/usr/local/bin/moonfire-nvr}"

# Process command line options
#
while getopts ":DS" opt; do
	case $opt in
	  S)	SKIP_APT=1
		;;
	  :)
		echo "Option -$OPTARG requires an argument." >&2
		exit 1
		;;
	  \?)
		echo "Invalid option: -$OPTARG" >&2
		exit 1
		;;
	esac
done

# Setup all apt packages we need
#
echo 'Preparing and downloading packages we need...'; echo
if [ "${SKIP_APT:-0}" != 1 ]; then
	sudo apt-get update
	sudo apt-get install \
		build-essential \
		libavcodec-dev \
		libavformat-dev \
		libavutil-dev \
		libncurses5-dev \
		libncursesw5-dev \
		libsqlite3-dev
fi

# Check if binary is installed. Setup for build if it is not
#
if [ ! -x "${SERVICE_BIN}" ]; then
	echo "Binary not installed, building..."; echo
	if ! cargo --version; then
		echo "cargo not installed/working."
		echo "Install a nightly Rust (see http://rustup.us) first."
		echo
		exit 1
	fi
	if ! cargo test; then
		echo "test failed. Try to run the following manually for more info"
		echo "RUST_TEST_THREADS=1 cargo test --verbose"
		echo
		exit 1
	fi
	if ! cargo build --release; then
		echo "Build failed."
		echo "RUST_TEST_THREADS=1 cargo build --release --verbose"
		echo
		exit 1
	fi
	sudo install -m 755 target/release/moonfire-nvr ${SERVICE_BIN}
	if [ -x "${SERVICE_BIN}" ]; then
		echo "Binary installed..."; echo
	else
		echo "Build failed to install..."; echo
		exit 1
	fi
fi

# Create user and groups
#
echo 'Create user/group and directories we need...'; echo
sudo addgroup --quiet --system ${NVR_GROUP}
sudo adduser --quiet --system ${NVR_USER} --ingroup "${NVR_GROUP}" --home "${NVR_HOME}"
if [ ! -d "${NVR_HOME}" ]; then
	sudo mkdir "${NVR_HOME}"
fi
sudo chown ${NVR_USER}:${NVR_GROUP} "${NVR_HOME}"

# Prepare samples directory
#
if [ -z "${SAMPLES_DIR}" ]; then
	SAMPLES_PATH="${NVR_HOME}/${SAMPLES_DIR_NAME}"
	if [ ! -d "${SAMPLES_PATH}" ]; then
		sudo -u ${NVR_USER} -H mkdir "${SAMPLES_PATH}"
	else
		chown -R ${NVR_USER}.${NVR_USER} "${SAMPLES_PATH}"
	fi
else
	SAMPLES_PATH="${SAMPLES_DIR}"
	if [ ! -d "${SAMPLES_PATH}" ]; then
		sudo mkdir "${SAMPLES_PATH}"
		echo ">>> Do not forget to edit /etc/fstab to mount samples media"; echo
	fi
	chown -R ${NVR_USER}.${NVR_USER} "${SAMPLES_PATH}"
fi


# Prepare for sqlite directory and set schema into db
#
echo 'Create database...'; echo
if [ ! -d "${DB_DIR}" ]; then
	sudo -u ${NVR_USER} -H mkdir "${DB_DIR}"
fi
sudo -u ${NVR_USER} -H ${SERVICE_BIN} init --db-dir="${DB_DIR}"

# Prepare service files
#
SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
sudo sh -c "cat > ${SERVICE_PATH}" <<NVR_EOF
[Unit]
Description=${SERVICE_DESC}
After=network-online.target

[Service]
ExecStart=${SERVICE_BIN} run \\
    --sample-file-dir=${SAMPLES_PATH} \\
    --db-dir=${DB_DIR} \\
    --http-addr=0.0.0.0:${NVR_PORT}
Environment=TZ=:/etc/localtime
Environment=MOONFIRE_FORMAT=google-systemd
Environment=MOONFIRE_LOG=info
Environment=RUST_BACKTRACE=1
Type=simple
User=${NVR_USER}
Nice=-20
Restart=on-abnormal
CPUAccounting=true
MemoryAccounting=true
BlockIOAccounting=true

[Install]
WantedBy=multi-user.target
NVR_EOF

# Configure and start service
#
if [ -f "${SERVICE_PATH}" ]; then
	echo 'Configuring system daemon...'; echo
	sudo systemctl daemon-reload
	sudo systemctl enable ${SERVICE_NAME}
	sudo systemctl restart ${SERVICE_NAME}
	echo 'Getting system daemon status...'; echo
	sudo systemctl status ${SERVICE_NAME}
fi
