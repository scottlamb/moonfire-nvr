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
RUSTC_MIN_VERSION="1.21"
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

read_lines()
{
	LINES_READ=()
	while read -r line; do
		LINES_READ+=("$line")
	done
}

catPrefix()
{
	sed -e "s/^/$2/" < "$1"
}

mkdir_moonfire()
{
	sudo -u "${NVR_USER}" -H mkdir "$@"
}

echo_multi()
{
	local prefix=''
	local plus=''

	while [[ $# -gt 0 ]]; do
		case "$1" in
			# Define a prefix for each line
			-p) shift; prefix="$1"; shift ;;
			# Provide extra empty line at end
			-x) shift; plus=1 ;;
			# Insert contents of LINES_READ here
			# Only works as leading option
			-L) shift; set -- "${LINES_READ[@]}" "$@" ;;
			# Stop processing options
			-) shift; break ;;
			# Non option break out
			*) break ;;
		esac
	done

	local A=("$@")
	for l in "${A[@]/#/$prefix}"; do
		echo "$l"
	done
	[ -n "$plus" ] && echo
}

echo_stderr()
{
	echo_multi "$@" 1>&2
}

echo_info()
{
	echo_multi -x -p '>>> ' "$@"
}


echo_warn()
{
	echo_multi -p 'WARNING: ' "$@" 1>&2
}

echo_error()
{
	echo_multi -p 'ERROR: ' "$@" 1>&2
}

echo_fatal()
{
	echo_error "$@"
	exit 1;
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
	NVR_USER="${NVR_USER:-moonfire-nvr}"
	NVR_GROUP="${NVR_GROUP:-$NVR_USER}"
	NVR_PORT="${NVR_PORT:-8080}"
	NVR_HOME_BASE="${NVR_HOME_BASE:-/var/lib}"
	NVR_HOME="${NVR_HOME_BASE}/${NVR_USER}"
	DB_NAME="${DB_NAME:-db}"
	DB_DIR="${DB_DIR:-$NVR_HOME/${DB_NAME}}"
	SAMPLE_FILE_DIR="${SAMPLE_FILE_DIR:-sample}"
	SAMPLE_MEDIA_DIR="${SAMPLE_MEDIA_DIR:-$NVR_HOME}"
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
			SAMPLE_FILE_DIR=$SAMPLE_FILE_DIR
			#SAMPLE_MEDIA_DIR=/mount/media
			SERVICE_NAME=$SERVICE_NAME
			SERVICE_DESC="$SERVICE_DESC"
EOF_CONFIG
		echo_info -x "File prep.config newly created. Inspect and change as necessary." \
				"When done, re-run this setup script. Currently it contains:"
		catPrefix "${MOONFIRE_DIR}/prep.config" "    "
		echo_info -x
		exit 0
	else
		echo_info -x "Setting up with variables:"
		catPrefix "${MOONFIRE_DIR}/prep.config" "    "
		echo_info -x
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

sudo_warn()
{
	echo_warn -x -p '!!!!!     ' \
		'------------------------------------------------------------------------------' \
		'During this script you may be asked to input your root password' \
		'This is for the purpose of using the sudo command and is necessary to complete' \
		'the script successfully.' \
		'------------------------------------------------------------------------------'
}


# Prepare for sqlite directory and set schema into db
#
setup_db()
{
	if [ ! -d "${DB_DIR}" ]; then
		echo_info -x 'Create database directory...'
		mkdir_moonfire -p "${DB_DIR}"
	fi
	echo_info -x 'Ensure database is initialized...'
	sudo -u "${NVR_USER}" -H -- "${SERVICE_BIN}" init --db-dir="${DB_DIR}"
}

# Make sure all sample directories and files owned by moonfire
#
fix_ownership()
{
	sudo chown -R ${NVR_USER}.${NVR_USER} "$1"
	echo_info -x "Fix ownership of files in \"$1\"..."
}

# Make sure samples directory is ready
#
prep_sample_file_dir()
{
	if [ -z "${SAMPLE_MEDIA_DIR}" ]; then
		echo_fatal -x "SAMPLE_MEDIA_DIR variable not configured. Check configuration."
		exit 1
	fi
	SAMPLE_FILE_PATH="${SAMPLE_MEDIA_DIR}/${SAMPLE_FILE_DIR}"
	if [ "${SAMPLE_FILE_PATH##${NVR_HOME}}" != "${SAMPLE_FILE_PATH}" ]; then
		# Under the home directory, create if not there
		if [ ! -d "${SAMPLE_FILE_PATH}" ]; then
			echo_info -x "Created sample file directory: \"$SAMPLE_FILE_PATH\"..."
			mkdir_moonfire -p "${SAMPLE_FILE_PATH}"
		fi
	else
		if [ ! -d "${SAMPLE_FILE_PATH}" ]; then
			read_lines <<-MSG1
	Samples directory $SAMPLE_FILE_PATH does not exist. 
	If a mounted file system, make sure /etc/fstab is properly configured, 
	and file system is mounted and directory created.
	MSG1
			echo_fatal -L
		fi
	fi
	fix_ownership "${SAMPLE_FILE_PATH}"
}

# Create user and groups if not there
#
prep_moonfire_user()
{
	echo_info -x "Create user/group and directories we need..."
	if ! groupExists "${NVR_GROUP}"; then
		sudo addgroup --quiet --system ${NVR_GROUP}
	fi
	if ! userExists "${NVR_USER}"; then
		sudo adduser --quiet --system ${NVR_USER} \
			--ingroup "${NVR_GROUP}" --home "${NVR_HOME}"
	fi
	if [ ! -d "${NVR_HOME}" ]; then
		sudo mkdir "${NVR_HOME}"
		sudo chown "${NVR_USER}:${NVR_GROUP}"  "${NVR_HOME}"
	fi
	sudo chown ${NVR_USER}:${NVR_GROUP} "${NVR_HOME}"
}

# Correct possible timezone issues
#
fix_localtime()
{
	if [ ! -L /etc/localtime ] && [ -f /etc/timezone ] &&
			[ -f "/usr/share/zoneinfo/`cat /etc/timezone`" ]; then
		echo_info -x "Correcting /etc/localtime setup issue..."
		sudo ln -sf /usr/share/zoneinfo/`cat /etc/timezone` /etc/localtime
	fi
}

pre_install_prep()
{
	prep_moonfire_user
	setup_db
	prep_sample_file_dir
	fix_localtime
}

