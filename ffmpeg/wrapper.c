// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2017 The Moonfire NVR Authors
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
/* vim: set sw=4 et: */

#include <libavcodec/avcodec.h>
#include <libavcodec/version.h>
#include <libavformat/avformat.h>
#include <libavformat/version.h>
#include <libavutil/avutil.h>
#include <libavutil/dict.h>
#include <libavutil/version.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdlib.h>

const int moonfire_ffmpeg_compiled_libavcodec_version = LIBAVCODEC_VERSION_INT;
const int moonfire_ffmpeg_compiled_libavformat_version = LIBAVFORMAT_VERSION_INT;
const int moonfire_ffmpeg_compiled_libavutil_version = LIBAVUTIL_VERSION_INT;

const int moonfire_ffmpeg_av_dict_ignore_suffix = AV_DICT_IGNORE_SUFFIX;

const int64_t moonfire_ffmpeg_av_nopts_value = AV_NOPTS_VALUE;

const int moonfire_ffmpeg_avmedia_type_video = AVMEDIA_TYPE_VIDEO;

const int moonfire_ffmpeg_av_codec_id_h264 = AV_CODEC_ID_H264;

const int moonfire_ffmpeg_averror_eof = AVERROR_EOF;

// Prior to libavcodec 58.9.100, multithreaded callers were expected to supply
// a lock callback. That release deprecated this API. It also introduced a
// FF_API_LOCKMGR #define to track its removal:
//
// * older builds (in which the lock callback is needed) don't define it.
// * middle builds (in which the callback is deprecated) define it as 1.
//   value of 1.
// * future builds (in which the callback removed) will define
//   it as 0.
//
// so (counterintuitively) use the lock manager when FF_API_LOCKMGR is
// undefined.

#ifndef FF_API_LOCKMGR
static int lock_callback(void **mutex, enum AVLockOp op) {
    switch (op) {
        case AV_LOCK_CREATE:
            *mutex = malloc(sizeof(pthread_mutex_t));
            if (*mutex == NULL)
                return -1;
            if (pthread_mutex_init(*mutex, NULL) != 0)
                return -1;
            break;
        case AV_LOCK_DESTROY:
            if (pthread_mutex_destroy(*mutex) != 0)
                return -1;
            free(*mutex);
            *mutex = NULL;
            break;
        case AV_LOCK_OBTAIN:
            if (pthread_mutex_lock(*mutex) != 0)
                return -1;
            break;
        case AV_LOCK_RELEASE:
            if (pthread_mutex_unlock(*mutex) != 0)
                return -1;
            break;
        default:
            return -1;
    }
    return 0;
}
#endif

void moonfire_ffmpeg_init(void) {
#ifndef FF_API_LOCKMGR
    if (av_lockmgr_register(&lock_callback) < 0) {
        abort();
    }
#endif
}

struct moonfire_ffmpeg_streams {
    AVStream** streams;
    size_t len;
};

struct moonfire_ffmpeg_data {
    uint8_t *data;
    size_t len;
};

struct moonfire_ffmpeg_streams moonfire_ffmpeg_fctx_streams(AVFormatContext *ctx) {
    struct moonfire_ffmpeg_streams s = {ctx->streams, ctx->nb_streams};
    return s;
}

AVPacket *moonfire_ffmpeg_packet_alloc(void) { return malloc(sizeof(AVPacket)); }
void moonfire_ffmpeg_packet_free(AVPacket *pkt) { free(pkt); }
bool moonfire_ffmpeg_packet_is_key(AVPacket *pkt) { return (pkt->flags & AV_PKT_FLAG_KEY) != 0; }
int64_t moonfire_ffmpeg_packet_pts(AVPacket *pkt) { return pkt->pts; }
void moonfire_ffmpeg_packet_set_dts(AVPacket *pkt, int64_t dts) { pkt->dts = dts; }
void moonfire_ffmpeg_packet_set_pts(AVPacket *pkt, int64_t pts) { pkt->pts = pts; }
void moonfire_ffmpeg_packet_set_duration(AVPacket *pkt, int dur) { pkt->duration = dur; }
int64_t moonfire_ffmpeg_packet_dts(AVPacket *pkt) { return pkt->dts; }
int moonfire_ffmpeg_packet_duration(AVPacket *pkt) { return pkt->duration; }
int moonfire_ffmpeg_packet_stream_index(AVPacket *pkt) { return pkt->stream_index; }
struct moonfire_ffmpeg_data moonfire_ffmpeg_packet_data(AVPacket *pkt) {
    struct moonfire_ffmpeg_data d = {pkt->data, pkt->size};
    return d;
}

AVCodecParameters *moonfire_ffmpeg_stream_codecpar(AVStream *stream) { return stream->codecpar; }
AVRational moonfire_ffmpeg_stream_time_base(AVStream *stream) { return stream->time_base; }

int moonfire_ffmpeg_codecpar_codec_id(AVCodecParameters *codecpar) { return codecpar->codec_id; }
int moonfire_ffmpeg_codecpar_codec_type(AVCodecParameters *codecpar) {
    return codecpar->codec_type;
}
struct moonfire_ffmpeg_data moonfire_ffmpeg_codecpar_extradata(AVCodecParameters *codecpar) {
    struct moonfire_ffmpeg_data d = {codecpar->extradata, codecpar->extradata_size};
    return d;
}
int moonfire_ffmpeg_codecpar_height(AVCodecParameters *codecpar) { return codecpar->height; }
int moonfire_ffmpeg_codecpar_width(AVCodecParameters *codecpar) { return codecpar->width; }
