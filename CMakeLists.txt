# This file is part of Moonfire NVR, a security camera digital video recorder.
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
# CMakeLists.txt: top-level definitions for building Moonfire NVR.

cmake_minimum_required(VERSION 3.0.2)
project(moonfire-nvr)

if(CMAKE_VERSION VERSION_LESS "3.1")
  set(CMAKE_CXX_FLAGS "--std=c++11 ${CMAKE_CXX_FLAGS}")
else()
  set(CMAKE_CXX_STANDARD 11)
endif()

set(CMAKE_CXX_FLAGS "-Wall -Werror -pedantic-errors ${CMAKE_CXX_FLAGS}")
set(CMAKE_CXX_FLAGS_DEBUG "${CMAKE_CXX_FLAGS_DEBUG} -ggdb")

#
# Dependencies.
#

# https://gflags.github.io/gflags/#cmake mentions a cmake module, but at
# least on Ubuntu 15.10, libgflags-dev does not include it. There's no
# pkgconfig either. Do this by hand.
find_library(GFLAGS_LIBRARIES gflags)
find_library(RE2_LIBRARIES re2)
find_library(PROFILER_LIBRARIES profiler)

# https://cmake.org/cmake/help/v3.0/module/FindPkgConfig.html
find_package(PkgConfig)
pkg_check_modules(FFMPEG REQUIRED libavutil libavcodec libavformat)
pkg_check_modules(LIBEVENT REQUIRED libevent>=2.1)
pkg_check_modules(GLOG REQUIRED libglog)
pkg_check_modules(OPENSSL REQUIRED libcrypto)
pkg_check_modules(SQLITE REQUIRED sqlite3)
pkg_check_modules(UUID REQUIRED uuid)

# Check if ffmpeg support "stimeout".
set(CMAKE_REQUIRED_INCLUDES ${FFMPEG_INCLUDES})
set(CMAKE_REQUIRED_LIBRARIES ${FFMPEG_LIBRARIES})
include(CheckCSourceRuns)
check_c_source_runs([[
#include <stdlib.h>
#include <libavutil/opt.h>
#include <libavformat/avformat.h>
int main(int argc, char **argv) {
  av_register_all();
  AVInputFormat *input = av_find_input_format("rtsp");
  const AVClass *klass = input->priv_class;
  const AVOption *opt =
      av_opt_find2(&klass, "stimeout", NULL, 0, AV_OPT_SEARCH_FAKE_OBJ, NULL);
  return (opt != NULL) ? EXIT_SUCCESS : EXIT_FAILURE;
}
]] HAVE_STIMEOUT)
if(NOT HAVE_STIMEOUT)
  message(WARNING [[
Your libavformat library lacks support for the "stimeout" rtsp option.
Moonfire NVR will not be able to detect network partitions or retry.
Consider installing a recent ffmpeg, from source if necessary.
]])
else()
  message(STATUS "libavformat library has support for \"stimeout\" - good.")
endif()

enable_testing()

# http://www.kaizou.org/2014/11/gtest-cmake/
find_package(Threads REQUIRED)
include(ExternalProject)
ExternalProject_Add(
    GMockProject
    URL "https://googlemock.googlecode.com/files/gmock-1.7.0.zip"
    URL_HASH "SHA1=f9d9dd882a25f4069ed9ee48e70aff1b53e3c5a5"
    INSTALL_COMMAND "")
ExternalProject_Get_Property(GMockProject source_dir binary_dir)
set(GTest_INCLUDE_DIR ${source_dir}/gtest/include)
add_library(GTest STATIC IMPORTED)
add_dependencies(GTest GMockProject)
set_target_properties(GTest PROPERTIES
    IMPORTED_LOCATION "${binary_dir}/gtest/${CMAKE_STATIC_LIBRARY_PREFIX}gtest${CMAKE_STATIC_LIBRARY_SUFFIX}"
    IMPORTED_LINK_INTERFACE_LIBRARIES "${CMAKE_THREAD_LIBS_INIT}")
set(GMock_INCLUDE_DIR ${source_dir}/include)
add_library(GMock STATIC IMPORTED)
add_dependencies(GMock GMockProject)
set_target_properties(GMock PROPERTIES
    IMPORTED_LOCATION "${binary_dir}/${CMAKE_STATIC_LIBRARY_PREFIX}gmock${CMAKE_STATIC_LIBRARY_SUFFIX}"
    IMPORTED_LINK_INTERFACE_LIBRARIES "${CMAKE_THREAD_LIBS_INIT}")

#
# Subdirectories.
#

add_subdirectory(src)
