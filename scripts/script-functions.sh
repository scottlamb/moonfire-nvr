#
# This file is part of Moonfire NVR, a security camera network video recorder.
# Copyright (C) 2016-2017 Scott Lamb <slamb@slamb.org>
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

if [ -z "$BASH_VERSION" ]; then
	echo "Script must run using bash (/bin/bash), not dash (/bin/sh)."
	exit 1
fi

# Useful constants
#
NODE_MIN_VERSION="8"
YARN_MIN_VERSION="1.0"
CARGO_MIN_VERSION="0.2"
RUSTC_MIN_VERSION="1.17"
FFMPEG_MIN_VERSION="55.1.101"
FFMPEG_RELEASE_VERSION="3.4"

normalizeDirPath()
{
	echo "$( cd -P "$1" && pwd )"
}

resolvePath()
{
	local d="$1"
	while [ -h "$d" ]; do # resolve $d until file no longer a symlink
		DIR="$( cd -P "$( dirname "$d" )" && pwd )"
		d="$(readlink "$d")"
		# if $d was rel symlink, resolve relative to path of symlink
		[[ "$d" != /* ]] && ="$DIR/$d"
	done 
	echo "$d"
}

functionsInit()
{
	local p="$(resolvePath "${BASH_SOURCE[0]}")"
	MOONFIRE_DIR="$(normalizeDirPath "`dirname "${p}"`/..")"
}

catPrefix()
{
	sed -e "s/^/$2/" < "$1"
}



# Read possible user config and then compute all derived environment
# variables used by the script(s).
#
initEnvironmentVars()
{
	test -z "${MOONFIRE_DIR}" && functionsInit
	if [ -r "${MOONFIRE_DIR}/prep.config" ]; then
		. "${MOONFIRE_DIR}/prep.config"
	fi
	SRC_DIR="$(resolvePath "$MOONFIRE_DIR/src")"
	NVR_USER="${NVR_USER:-moonfire-nvr}"
	NVR_GROUP="${NVR_GROUP:-$NVR_USER}"
	NVR_PORT="${NVR_PORT:-8080}"
	NVR_HOME_BASE="${NVR_HOME_BASE:-/var/lib}"
	NVR_HOME="${NVR_HOME_BASE}/${NVR_USER}"
	DB_NAME="${DB_NAME:-db}"
	DB_DIR="${DB_DIR:-$NVR_HOME}/${DB_NAME}"
	SAMPLES_DIR_NAME="${SAMPLES_DIR_NAME:-sample}"
	SAMPLES_MEDIA_DIR="${SAMPLES_MEDIA_DIR:-$NVR_HOME}"
	SERVICE_NAME="${SERVICE_NAME:-moonfire-nvr}"
	SERVICE_DESC="${SERVICE_DESC:-Moonfire NVR}"
	SERVICE_BIN="${SERVICE_BIN:-/usr/local/bin/moonfire-nvr}"
	LIB_DIR="${UI_DIR:-/usr/local/lib/moonfire-nvr}"
}

# Create file with confguration variables that are user changeable.
# Not all variables are included here, only the likely ones to be
# modified.
# If the file already exists, it will not be modified
# 
makePrepConfig()
{
	test -z "${MOONFIRE_DIR}" && functionsInit
	if [ ! -f "${MOONFIRE_DIR}/prep.config" ]; then
		cat >"${MOONFIRE_DIR}/prep.config" <<-EOF_CONFIG
			NVR_USER=$NVR_USER
			NVR_PORT=$NVR_PORT
			SAMPLES_DIR_NAME=$SAMPLES_DIR_NAME
			#SAMPLES_MEDIA_DIR=/mount/media
			SERVICE_NAME=$SERVICE_NAME
			SERVICE_DESC="$SERVICE_DESC"
EOF_CONFIG
		echo "File prep.config newly created. Inspect and change as necessary."
		echo "When done, re-run this setup script. Currently it contains:"
		catPrefix "${MOONFIRE_DIR}/prep.config" "    "
		exit 0
	else
		echo "Setting up with variables:"
		catPrefix "${MOONFIRE_DIR}/prep.config" "    "
		echo ""
	fi
}

# Extract version data from stdin, possibly grepping first against
# single argument.
#
extractVersion()
{
	local pattern="$1"

	if [ -n "${pattern}" ]; then
		grep "$pattern" | sed -e 's/[^0-9.]*\([0-9. ]*\).*/\1/' | tr -d ' '
	else
		sed -e 's/[^0-9.]*\([0-9. ]*\).*/\1/' | tr -d ' '
	fi
}

getAVersion()
{
	local v=`$1 $2 2>/dev/null | extractVersion`
	if [ "X${v}" = "X" ]; then echo "$3"; else echo "${v}"; fi
}

getVersion()
{
	getAVersion $1 --version $2
}

getClassicVersion()
{
	getAVersion $1 -version $2
}

versionAtLeast()
{
	v1=(${1//./ } 0 0 0); v1=("${v1[@]:0:3}")
	v2=(${2//./ } 0 0 0); v2=("${v2[@]:0:3}")
	
	for i in 0 1 2; do
		if [ "${v1[$i]}" -gt "${v2[$i]}" ]; then return 0; fi
		if [ "${v1[$i]}" -lt "${v2[$i]}" ]; then return 1; fi
	done
	return 0
}

initCargo()
{
	if [ -r ~/.cargo/env ]; then
		source ~/.cargo/env
	fi
}

userExists()
{
	return $(id -u "$1" >/dev/null 2>&1)
}

groupExists()
{
	return $(id -g "$1" >/dev/null 2>&1)
}

moonfire()
{
	case "$1" in
	start)
		sudo systemctl start "$2"
		;;
	stop)
		sudo systemctl stop "$2"
		;;
	esac
}

# Add/update cameras in the database
#
# $1: path to cameras.sql file
# $2: moonfire service name
# $3: moonfire user name
# $4: moonfire database path
#
addCameras()
{
	local cpath="${CAMERAS_PATH:-$1}"
	if [ -r "${cpath}" ]; then
		echo 'Add cameras...'; echo
		# Before adding cameras, must stop service
		moonfire stop "${SERVICE_NAME:-$2}" >/dev/null 2>&1
		sudo -u ${NVR_USER:-$3} -H sqlite3 "${DB_PATH:-$4}" < "${cpath}"
		moonfire start "${SERVICE_NAME:-$2}" >/dev/null 2>&1
	fi
}

echo_stderr()
{
	echo "$@" 1>&2
}

echo_warn()
{
	echo_stderr "WARNING: $@"
	echo_stderr
}

echo_fatal()
{
	echo_stderr "ERROR: $@"
	echo_stderr
	exit 1;
}
