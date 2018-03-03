#!/bin/bash -x
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

. `dirname ${BASH_SOURCE[0]}`/script-functions.sh

makePrepConfig
initEnvironmentVars


# Process command line options
#
while getopts ":f" opt; do
	case $opt in
	  f)    DONT_BUILD_FFMPEG=1
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
PKGS="build-essential pkg-config sqlite3"
#PKGS="$PKGS libavcodec-dev libavformat-dev libavutil-dev"
PKGS="$PKGS libncurses5-dev libncursesw5-dev"
PKGS="$PKGS libsqlite3-dev libssl-dev"

# Add yarn before NodeSource so it can all go in one update
#
yv=$(getVersion yarn "NA")
if [ ${yv} = "NA" ]; then
	curl -sS https://dl.yarnpkg.com/debian/pubkey.gpg |\
			 sudo apt-key add -
	echo "deb https://dl.yarnpkg.com/debian/ stable main" |\
			 sudo tee /etc/apt/sources.list.d/yarn.list
	PKGS="$PKGS yarn"
fi

# Check for minimum node version
#
nv=$(getVersion node 0)
if ! versionAtLeast "$nv" "$NODE_MIN_VERSION"; then
	# Nodesource will also make sure we have apt-transport-https
	# and will run apt-get-update when done
	#
	curl -sL https://deb.nodesource.com/setup_${NODE_MIN_VERSION}.x |
								sudo -E bash -
	PKGS="$PKGS nodejs"
	DO_UPDATE=0
else
	PKGS="$PKGS apt-transport-https"
fi

# Run apt-get update if still necessary
#
if [ ${DO_UPDATE:-1} ]; then sudo apt-get update -y; fi

# Install necessary pakackes
#
sudo apt-get install -y $PKGS
sudo apt-get autoremove -y

# Check for ffmpeg and install by building if necessary
# This needs to be done before building moonfire so it can
# find the right versions of the libraries.
#
ffv=`ffmpeg -version 2>/dev/null | extractVersion libavutil`
ffv=${ffv:-0}
if ! versionAtLeast "$ffv" "$FFMPEG_MIN_VERSION"; then
	if [ "${DONT_BUILD_FFMPEG:-0}" -ne 0 ]; then
		echo "ffmpeg version (${ffv}) installed is too old for moonfire."
		echo "Suggest you manually install at least version $FFMPEG_MIN_VERSION of libavutil."
		echo "ffmpeg versions 2.x and 3.x all should work."
	else
		OLDDIR=`pwd`
		cd ..
		if [ -d FFmpeg ]; then
			echo "Removing older FFmpeg directory..."; echo
			rm -fr FFmpeg
		fi
		echo "Fetching FFmpeg source..."; echo
		git clone --depth 1 -b "release/${FFMPEG_RELEASE_VERSION}" https://github.com/FFmpeg/FFmpeg.git
		cd FFmpeg
		pt=`uname -p 2>& /dev/null`
		if [ -z "${pt##*86*}" ]; then
			sudo apt-get install -y yasm
		fi
		./configure --enable-shared
		make
		sudo make install
		sudo ldconfig
		cd "$OLDDIR"
		OLDDIR=
	fi
else
	echo "FFmpeg is already usable..."; echo
fi

# If cargo appears installed, initialize for using it so rustc can be found
#
initCargo

# Make sure we have rust and cargo
rv=$(getVersion rustc 0.0)
if ! versionAtLeast "$rv" "$RUSTC_MIN_VERSION"; then
	echo "Installing latest rust and cargo..."; echo
	curl https://sh.rustup.rs -sSf | sh -s - -y
	initCargo
fi

cv=$(getVersion cargo "NA")
if [ ${cv} = "NA" ]; then
	echo "Cargo is not (properly) installed, but rust is."
	echo "Suggest you install the latest rustup, or manually install cargo."
	echo "Install using: curl https://sh.rustup.rs -sSf | sh -s -y"
	exit 1
fi

# Now make sure we have dev environment and tools for the UI portion
#
echo "Adding yarn components we need..."; echo
grep -qv \"webpack\" package.json && yarn add --dev webpack@${WEBPACK_MIN_VERSION}
echo "Installing all dependencies with yarn..."
yarn install

finish()
{
	if [ -z "${OLDDIR}" ]; then
		cd "${OLDDIR}"
	fi
}
trap finish EXIT

# Create user and groups if not there
#
echo
echo "Create user/group and directories we need..."; echo
if ! groupExists "${NVR_GROUP}"; then
	sudo addgroup --quiet --system ${NVR_GROUP}
fi
if ! userExists "${NVR_USER}"; then
	sudo adduser --quiet --system ${NVR_USER} \
		--ingroup "${NVR_GROUP}" --home "${NVR_HOME}"
fi
if [ ! -d "${NVR_HOME}" ]; then
        sudo mkdir "${NVR_HOME}"
fi
sudo chown ${NVR_USER}:${NVR_GROUP} "${NVR_HOME}"


# Correct possible timezone issues
#
echo "Correcting possible /etc/localtime setup issue..."; echo
if [ ! -L /etc/localtime ] && [ -f /etc/timezone ] &&
                [ -f "/usr/share/zoneinfo/`cat /etc/timezone`" ]; then
        sudo rm /etc/localtime
        sudo ln -s /usr/share/zoneinfo/`cat /etc/timezone` /etc/localtime
fi


# Prepare for sqlite directory and set schema into db
#
DB_NAME=db
DB_PATH="${DB_DIR}/${DB_NAME}"
if [ ! -d "${DB_DIR}" ]; then
        echo 'Create database...'; echo
        sudo -u "${NVR_USER}" -H mkdir "${DB_DIR}"
fi
if [ ! -f "${DB_PATH}" ]; then
        sudo -u "${NVR_USER}" -H sqlite3 "${DB_PATH}" < "${SRC_DIR}/schema.sql"
fi

CAMERAS_PATH="${MOONFIRE_DIR}/cameras.sql"
if [ ! -r "${CAMERAS_PATH}" ]; then
	CAMERAS_PATH="${MOONFIRE_DIR}/../cameras.sql"
	if [ ! -r "${CAMERAS_PATH}" ]; then
		CAMERAS_PATH=
	fi
fi
if [ ! -z "${CAMERAS_PATH}" ]; then
	echo "Adding camera confguration to db..."; echo
	addCameras
else
	echo "!!!!! No cameras auto configured. Use \"moonfire-nvr config\" to do it later..."; echo
fi


# Make sure samples directory is ready
#
if [ -z "${SAMPLES_MEDIA_DIR}" ]; then
	echo "SAMPLES_MEDIA_DIR variable not configured. Check configuration."
	exit 1
fi
SAMPLES_PATH="${SAMPLES_MEDIA_DIR}/${SAMPLES_DIR_NAME}"
if [ "${SAMPLES_PATH##${NVR_HOME}}" != "${SAMPLES_PATH}" ]; then
	# Under the home directory, create if not there
	if [ ! -d "${SAMPLES_PATH}" ]; then
		echo "Created samples directory: $SAMPLES_PATH"; echo
		sudo -u ${NVR_USER} -H mkdir "${SAMPLES_PATH}"
	fi
else
	if [ ! -d "${SAMPLES_PATH}" ]; then
		cat <<-MSG1
!!!!!
Samples directory $SAMPLES_PATH does not exist.
If a mounted file system, make sure /etc/fstab is properly configured,
and file system is mounted and directory created.
!!!!!
MSG1
		exit 1
	fi
fi
# Make sure all sample directories and files owned by moonfire
#
sudo chown -R ${NVR_USER}.${NVR_USER} "${SAMPLES_PATH}"
echo "Fix ownership of sample files..."; echo


cat <<-'INSTRUCTIONS'
Unless there are errors above, everything you need should have been installed
and you are now ready to build, install, and then use moonfire.

Build by executing the script: scripts/build.sh
Install by executing the script: scripts/install.sh (run automatically by build
step).
INSTRUCTIONS
