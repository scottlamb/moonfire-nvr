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
// ffmpeg.cc: See ffmpeg.h for description.

#include "ffmpeg.h"

#include <mutex>

extern "C" {
#include <libavutil/buffer.h>
#include <libavutil/mathematics.h>
#include <libavutil/version.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/version.h>
#include <libavformat/version.h>
}  // extern "C"

#include <gflags/gflags.h>

#include "string.h"

// libav lacks this ffmpeg constant.
#ifndef AV_ERROR_MAX_STRING_SIZE
#define AV_ERROR_MAX_STRING_SIZE 64
#endif

DEFINE_int32(avlevel, AV_LOG_INFO,
             "maximum logging level for ffmpeg/libav; "
             "higher levels will be ignored.");

namespace moonfire_nvr {

namespace {

std::string AvError2Str(re2::StringPiece function, int err) {
  char str[AV_ERROR_MAX_STRING_SIZE];
  if (av_strerror(err, str, sizeof(str)) == 0) {
    return StrCat(function, ": ", str);
  }
  return StrCat(function, ": unknown error ", err);
}

struct Dictionary {
  Dictionary() {}
  Dictionary(const Dictionary &) = delete;
  Dictionary &operator=(const Dictionary &) = delete;
  ~Dictionary() { av_dict_free(&dict); }

  bool Set(const char *key, const char *value, std::string *error_message) {
    int ret = av_dict_set(&dict, key, value, 0);
    if (ret < 0) {
      *error_message = AvError2Str("av_dict_set", ret);
      return false;
    }
    return true;
  }

  bool size() const { return av_dict_count(dict); }

  AVDictionary *dict = nullptr;
};

google::LogSeverity GlogLevelFromAvLevel(int avlevel) {
  if (avlevel >= AV_LOG_INFO) {
    return google::GLOG_INFO;
  } else if (avlevel >= AV_LOG_WARNING) {
    return google::GLOG_WARNING;
  } else if (avlevel > AV_LOG_PANIC) {
    return google::GLOG_ERROR;
  } else {
    return google::GLOG_FATAL;
  }
}

void AvLogCallback(void *avcl, int avlevel, const char *fmt, va_list vl) {
  if (avlevel > FLAGS_avlevel) {
    return;
  }

  // google::LogMessage expects a "file" and "line" to be prefixed to the
  // log message, like so:
  //
  // W1210 11:00:32.224936 28739 ffmpeg_rtsp:0] Estimating duration ...
  //                             ^file       ^line
  //
  // Normally this is filled in via the __FILE__ and __LINE__
  // C preprocessor macros. In this case, try to fill in something useful
  // based on the information ffmpeg supplies.
  std::string file("ffmpeg");
  if (avcl != nullptr) {
    auto *avclass = *reinterpret_cast<AVClass **>(avcl);
    file.push_back('_');
    file.append(avclass->item_name(avcl));
  }
  char line[512];
  vsnprintf(line, sizeof(line), fmt, vl);
  google::LogSeverity glog_level = GlogLevelFromAvLevel(avlevel);
  google::LogMessage(file.c_str(), 0, glog_level).stream() << line;
}

int AvLockCallback(void **mutex, enum AVLockOp op) {
  auto typed_mutex = reinterpret_cast<std::mutex **>(mutex);
  switch (op) {
    case AV_LOCK_CREATE:
      LOG_IF(DFATAL, *typed_mutex != nullptr)
          << "creating mutex over existing value.";
      *typed_mutex = new std::mutex;
      break;
    case AV_LOCK_DESTROY:
      delete *typed_mutex;
      *typed_mutex = nullptr;
      break;
    case AV_LOCK_OBTAIN:
      (*typed_mutex)->lock();
      break;
    case AV_LOCK_RELEASE:
      (*typed_mutex)->unlock();
      break;
  }
  return 0;
}

std::string StringifyVersion(int version_int) {
  return StrCat((version_int >> 16) & 0xFF, ".", (version_int >> 8) & 0xFF, ".",
                (version_int)&0xFF);
}

void LogVersion(const char *library_name, int compiled_version,
                int running_version, const char *configuration) {
  LOG(INFO) << library_name << ": compiled with version "
            << StringifyVersion(compiled_version) << ", running with version "
            << StringifyVersion(running_version)
            << ", configuration: " << configuration;
}

class RealInputVideoPacketStream : public InputVideoPacketStream {
 public:
  RealInputVideoPacketStream() {
    ctx_ = CHECK_NOTNULL(avformat_alloc_context());
  }

  RealInputVideoPacketStream(const RealInputVideoPacketStream &) = delete;
  RealInputVideoPacketStream &operator=(const RealInputVideoPacketStream &) =
      delete;

  ~RealInputVideoPacketStream() final {
    avformat_close_input(&ctx_);
    avformat_free_context(ctx_);
  }

  bool GetNext(VideoPacket *pkt, std::string *error_message) final {
    while (true) {
      av_packet_unref(pkt->pkt());
      int ret = av_read_frame(ctx_, pkt->pkt());
      if (ret != 0) {
        if (ret == AVERROR_EOF) {
          error_message->clear();
        } else {
          *error_message = AvError2Str("av_read_frame", ret);
        }
        return false;
      }
      if (pkt->pkt()->stream_index != stream_index_) {
        VLOG(3) << "Ignoring packet for stream " << pkt->pkt()->stream_index
                << "; only interested in " << stream_index_;
        continue;
      }
      VLOG(3) << "Read packet with pts=" << pkt->pkt()->pts
              << ", dts=" << pkt->pkt()->dts << ", key=" << pkt->is_key();
      return true;
    }
  }

  const AVStream *stream() const final { return ctx_->streams[stream_index_]; }

 private:
  friend class RealVideoSource;
  int64_t min_next_pts_ = std::numeric_limits<int64_t>::min();
  int64_t min_next_dts_ = std::numeric_limits<int64_t>::min();
  AVFormatContext *ctx_ = nullptr;  // owned.
  int stream_index_ = -1;
};

class RealVideoSource : public VideoSource {
 public:
  RealVideoSource() {
    CHECK_GE(0, av_lockmgr_register(&AvLockCallback));
    av_log_set_callback(&AvLogCallback);
    av_register_all();
    avformat_network_init();
    LogVersion("avutil", LIBAVUTIL_VERSION_INT, avutil_version(),
               avutil_configuration());
    LogVersion("avformat", LIBAVFORMAT_VERSION_INT, avformat_version(),
               avformat_configuration());
    LogVersion("avcodec", LIBAVCODEC_VERSION_INT, avcodec_version(),
               avcodec_configuration());
  }

  std::unique_ptr<InputVideoPacketStream> OpenRtsp(
      const std::string &url, std::string *error_message) final {
    std::unique_ptr<InputVideoPacketStream> stream;
    Dictionary open_options;
    if (!open_options.Set("rtsp_transport", "tcp", error_message) ||
        // https://trac.ffmpeg.org/ticket/5018 workaround attempt.
        !open_options.Set("probesize", "262144", error_message) ||
        !open_options.Set("user-agent", "moonfire-nvr", error_message) ||
        // 10-second socket timeout, in microseconds.
        !open_options.Set("stimeout", "10000000", error_message)) {
      return stream;
    }

    stream = OpenCommon(url, &open_options.dict, error_message);
    if (stream == nullptr) {
      return stream;
    }

    // Discard the first packet.
    LOG(INFO) << "Discarding the first packet to work around "
                 "https://trac.ffmpeg.org/ticket/5018";
    VideoPacket dummy;
    if (!stream->GetNext(&dummy, error_message)) {
      stream.reset();
    }

    return stream;
  }

  std::unique_ptr<InputVideoPacketStream> OpenFile(
      const std::string &filename, std::string *error_message) final {
    AVDictionary *open_options = nullptr;
    return OpenCommon(filename, &open_options, error_message);
  }

 private:
  std::unique_ptr<InputVideoPacketStream> OpenCommon(
      const std::string &source, AVDictionary **dict,
      std::string *error_message) {
    std::unique_ptr<RealInputVideoPacketStream> stream(
        new RealInputVideoPacketStream);

    int ret = avformat_open_input(&stream->ctx_, source.c_str(), nullptr, dict);
    if (ret != 0) {
      *error_message = AvError2Str("avformat_open_input", ret);
      return std::unique_ptr<InputVideoPacketStream>();
    }

    if (av_dict_count(*dict) != 0) {
      std::vector<std::string> ignored;
      AVDictionaryEntry *ent = nullptr;
      while ((ent = av_dict_get(*dict, "", ent, AV_DICT_IGNORE_SUFFIX)) !=
             nullptr) {
        ignored.push_back(StrCat(ent->key, "=", ent->value));
      }
      LOG(WARNING) << "avformat_open_input ignored " << ignored.size()
                   << " options: " << Join(ignored, ", ");
    }

    ret = avformat_find_stream_info(stream->ctx_, nullptr);
    if (ret < 0) {
      *error_message = AvError2Str("avformat_find_stream_info", ret);
      return std::unique_ptr<InputVideoPacketStream>();
    }

    // Find the video stream.
    for (unsigned int i = 0; i < stream->ctx_->nb_streams; ++i) {
      if (stream->ctx_->streams[i]->codec->codec_type == AVMEDIA_TYPE_VIDEO) {
        VLOG(1) << "Video stream index is " << i;
        stream->stream_index_ = i;
        break;
      }
    }
    if (stream->stream() == nullptr) {
      *error_message = StrCat("no video stream");
      return std::unique_ptr<InputVideoPacketStream>();
    }

    return std::unique_ptr<InputVideoPacketStream>(stream.release());
  }
};

}  // namespace

VideoSource *GetRealVideoSource() {
  static auto *real_video_source = new RealVideoSource;  // never deleted.
  return real_video_source;
}

}  // namespace moonfire_nvr
