#!/bin/bash
#
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2016-17 Scott Lamb <slamb@slamb.org>
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

. `dirname ${BASH_SOURCE[0]}`/script-functions.sh

# Determine directory path of this script
#
initEnvironmentVars

# Process command line options
#
while getopts ":s" opt; do
	case $opt in
	  s)    NO_SERVICE=1
		;;
	  :)
		echo_fatal "Option -$OPTARG requires an argument."
		;;
	  \?)
		echo_fatal "Invalid option: -$OPTARG"
		;;
	esac
done

sudo_warn

sudo install -m 755 target/release/moonfire-nvr ${SERVICE_BIN}
if [ -x "${SERVICE_BIN}" ]; then
	echo_info -x "Server Binary installed..."
else
	echo_info -x "Server build failed to install..."
	exit 1
fi
if [ ! -d "${LIB_DIR}" ]; then
	sudo mkdir "${LIB_DIR}"
fi
if [ -d "ui-dist" ]; then
	sudo mkdir -p "${LIB_DIR}/ui"
	sudo cp -R ui-dist/. "${LIB_DIR}/ui/"
	sudo chown -R ${NVR_USER}:${NVR_GROUP} "${LIB_DIR}/ui/"
	echo_info -x "Server UI installed..."
else
	echo_fatal -x "Server UI failed to build or install..."
fi

if [ "${NO_SERVICE:-0}" != 0 ]; then
	echo_info -x "Not configuring systemctl service. Done"
	exit 0
fi

# Make sure user was created
#
if ! userExists "${NVR_USER}"; then
	echo_fatal -x "NVR_USER=${NVR_USER} was not created. Do so manually or run the setup script."
fi

pre_install_prep


# Prepare service files for socket when using priviliged port
#
SOCKET_SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}.socket"
if [ $NVR_PORT -lt 1024 ]; then
	echo_fatal -x "NVR_PORT ($NVR_PORT) < 1024: priviliged ports not supported"
fi

# Prepare service files for moonfire
#
SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
sudo sh -c "cat > ${SERVICE_PATH}" <<NVR_EOF
[Unit]
Description=${SERVICE_DESC}
After=network-online.target

[Service]
ExecStart=${SERVICE_BIN} run \\
    --db-dir=${DB_DIR} \\
    --ui-dir=${LIB_DIR}/ui \\
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
	echo_info -x 'Configuring system daemon...'
	sudo systemctl daemon-reload
	sudo systemctl enable ${SERVICE_NAME}
	sudo systemctl restart ${SERVICE_NAME}
	echo_info -x 'Getting system daemon status...'
	sudo systemctl status ${SERVICE_NAME}
fi
