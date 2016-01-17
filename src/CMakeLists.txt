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

include_directories(${CMAKE_CURRENT_BINARY_DIR})
PROTOBUF_GENERATE_CPP(PROTO_SRCS PROTO_HDRS config.protodevel)

set(MOONFIRE_DEPS
    ${FFMPEG_LIBRARIES}
    ${LIBEVENT_LIBRARIES}
    ${GFLAGS_LIBRARIES}
    ${GLOG_LIBRARIES}
    ${OPENSSL_LIBRARIES}
    ${PROFILER_LIBRARIES}
    ${PROTOBUF_LIBRARIES}
    ${RE2_LIBRARIES}
    ${SQLITE_LIBRARIES}
    ${UUID_LIBRARIES})

set(MOONFIRE_NVR_SRCS
    coding.cc
    crypto.cc
    ffmpeg.cc
    filesystem.cc
    h264.cc
    http.cc
    moonfire-db.cc
    moonfire-nvr.cc
    mp4.cc
    profiler.cc
    recording.cc
    sqlite.cc
    string.cc
    time.cc
    uuid.cc
    web.cc)

add_library(moonfire-nvr-lib ${MOONFIRE_NVR_SRCS} ${PROTO_SRCS} ${PROTO_HDRS})
target_link_libraries(moonfire-nvr-lib ${MOONFIRE_DEPS})

add_executable(moonfire-nvr moonfire-nvr-main.cc)
target_link_libraries(moonfire-nvr moonfire-nvr-lib)
install_programs(/bin FILES moonfire-nvr)

# Tests.
include_directories(${GTest_INCLUDE_DIR})
include_directories(${GMock_INCLUDE_DIR})

foreach(test coding crypto h264 http moonfire-nvr mp4 recording sqlite string)
  add_executable(${test}-test ${test}-test.cc testutil.cc)
  target_link_libraries(${test}-test GTest GMock moonfire-nvr-lib)
  add_test(NAME ${test}-test
           COMMAND ${test}-test
           WORKING_DIRECTORY ${CMAKE_BINARY_DIR})
endforeach(test)
