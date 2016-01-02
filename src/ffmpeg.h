// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// ffmpeg.h: ffmpeg (or libav) wrappers for operations needed by moonfire_nvr.
// This is not a general-purpose wrapper. It makes assumptions about the
// data we will be operated on and the desired operations, such as:
//
// * The input should contain no "B" frames (bi-directionally predicted
//   pictures) and thus input frames should be strictly in order of ascending
//   PTS as well as DTS.
//
// * Only video frames are of interest.

#ifndef MOONFIRE_NVR_FFMPEG_H
#define MOONFIRE_NVR_FFMPEG_H

#include <limits>
#include <memory>
#include <string>

#include <glog/logging.h>

extern "C" {
#include <libavformat/avformat.h>
}  // extern "C"

namespace moonfire_nvr {

// An encoded video packet.
class VideoPacket {
 public:
  VideoPacket() { av_init_packet(&pkt_); }
  VideoPacket(const VideoPacket &) = delete;
  VideoPacket &operator=(const VideoPacket &) = delete;
  ~VideoPacket() { av_packet_unref(&pkt_); }

  // Returns iff this packet represents a key frame.
  //
  // (A key frame is one that can be decoded without previous frames.)
  //
  // PRE: this packet is valid, as if it has been filled by
  // InputVideoPacketStream::Next.
  bool is_key() const { return (pkt_.flags & AV_PKT_FLAG_KEY) != 0; }

  int64_t pts() const { return pkt_.pts; }

  AVPacket *pkt() { return &pkt_; }

 private:
  AVPacket pkt_;
};

// An input stream of (still-encoded) video packets.
class InputVideoPacketStream {
 public:
  InputVideoPacketStream() {}
  InputVideoPacketStream(const InputVideoPacketStream &) = delete;
  InputVideoPacketStream &operator=(const InputVideoPacketStream &) = delete;

  // Closes the stream.
  virtual ~InputVideoPacketStream() {}

  // Get the next packet.
  //
  // Returns true iff one is available, false on EOF or failure.
  // |error_message| will be filled on failure, empty on EOF.
  //
  // PRE: the stream is healthy: there was no prior Close() call or GetNext()
  // failure.
  virtual bool GetNext(VideoPacket *pkt, std::string *error_message) = 0;

  // Returns the video stream.
  virtual const AVStream *stream() const = 0;
};

// A class which opens streams.
// There's one of these for proudction use; see GetRealVideoSource().
// It's an abstract class for testability.
class VideoSource {
 public:
  virtual ~VideoSource() {}

  // Open the given RTSP URL, accessing the first video stream.
  //
  // The RTSP URL will be opened with TCP and a hardcoded socket timeout.
  //
  // The first frame will be automatically discarded as a bug workaround.
  // https://trac.ffmpeg.org/ticket/5018
  //
  // Returns success, filling |error_message| on failure.
  //
  // PRE: closed.
  virtual std::unique_ptr<InputVideoPacketStream> OpenRtsp(
      const std::string &url, std::string *error_message) = 0;

  // Open the given video file, accessing the first video stream.
  //
  // Returns the stream. On failure, returns nullptr and fills
  // |error_message|.
  virtual std::unique_ptr<InputVideoPacketStream> OpenFile(
      const std::string &filename, std::string *error_message) = 0;
};

// Returns a VideoSource for production use, which will never be deleted.
VideoSource *GetRealVideoSource();

class OutputVideoPacketStream {
 public:
  OutputVideoPacketStream() {}
  OutputVideoPacketStream(const OutputVideoPacketStream &) = delete;
  OutputVideoPacketStream &operator=(const OutputVideoPacketStream &) = delete;

  ~OutputVideoPacketStream() { Close(); }

  bool OpenFile(const std::string &filename,
                const InputVideoPacketStream &input,
                std::string *error_message);

  bool Write(VideoPacket *pkt, std::string *error_message);

  void Close();

  bool is_open() const { return ctx_ != nullptr; }
  AVRational time_base() const { return stream_->time_base; }

 private:
  int64_t key_frames_written_ = -1;
  int64_t frames_written_ = -1;
  int64_t min_next_dts_ = std::numeric_limits<int64_t>::min();
  int64_t min_next_pts_ = std::numeric_limits<int64_t>::min();
  AVFormatContext *ctx_ = nullptr;  // owned.
  AVStream *stream_ = nullptr;      // ctx_ owns.
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_FFMPEG_H
