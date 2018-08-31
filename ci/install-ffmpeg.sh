#!/bin/bash -e
if [ ! -f ffmpeg-${FFMPEG_VERSION}/configure ]; then
  wget https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz
  tar xf ffmpeg-${FFMPEG_VERSION}.tar.xz
fi
cd ffmpeg-${FFMPEG_VERSION}
./configure --enable-shared
make --jobs=2
sudo make install --jobs=2
sudo ldconfig

# The build log varies with each invocation; remove it to improve cachability.
rm -f ffbuild/config.log
