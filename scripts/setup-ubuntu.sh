#!/bin/bash
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

initEnvironmentVars
makePrepConfig
export DEBIAN_FRONTEND=noninteractive


sudo_warn

# Setup all apt packages we need
#
echo_info -x 'Preparing and downloading packages we need...'
PKGS="build-essential \
      libavcodec-dev \
      libavformat-dev \
      libavutil-dev \
      libncurses5-dev \
      libncursesw5-dev \
      libsqlite3-dev \
      libssl-dev \
      pkgconf \
      sqlite3 \
      tzdata"

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

# Install necessary packages
#
sudo apt-get install -y $PKGS
sudo apt-get autoremove -y
echo_info -x

# If cargo appears installed, initialize for using it so rustc can be found
#
initCargo

# Make sure we have rust and cargo
rv=$(getVersion rustc 0.0)
if ! versionAtLeast "$rv" "$RUSTC_MIN_VERSION"; then
	echo_info -x "Installing latest rust and cargo..."
	curl https://sh.rustup.rs -sSf | sh -s - -y
	initCargo
fi

cv=$(getVersion cargo "NA")
if [ ${cv} = "NA" ]; then
	echo_fatal -x "Cargo is not (properly) installed, but rust is." \
		"Suggest you install the latest rustup, or manually install cargo."
		"Install using: curl https://sh.rustup.rs -sSf | sh -s -y"
fi

# Now make sure we have dev environment and tools for the UI portion
#
echo_info -x "Installing all dependencies with yarn..."
yarn install
echo_info -x

finish()
{
	if [ -z "${OLDDIR}" ]; then
		cd "${OLDDIR}"
	fi
}
trap finish EXIT

# Rest of prep
#
pre_install_prep


read_lines <<-'INSTRUCTIONS'
Unless there are errors above, everything you need should have been installed
and you are now ready to build, install, and then use moonfire.

Build by executing the script: scripts/build.sh
Install by executing the script: scripts/install.sh (run automatically by build
step).
INSTRUCTIONS
echo_info -x -p '    ' -L

