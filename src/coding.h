// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 Lamb <slamb@slamb.org>
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
// coding.h: Binary encoding/decoding.

#ifndef MOONFIRE_NVR_CODING_H
#define MOONFIRE_NVR_CODING_H

#include <endian.h>
#include <stdint.h>

#include <string>

#include <re2/stringpiece.h>

namespace moonfire_nvr {

namespace internal {

void AppendVar32Slow(uint32_t in, std::string *out);
bool DecodeVar32Slow(re2::StringPiece *in, uint32_t *out,
                     std::string *error_message);

}  // namespace internal

// Endianness conversion.

#if __BYTE_ORDER == __LITTLE_ENDIAN
// XXX: __builtin_bswap64 doesn't compile on gcc 5.2.1 with an error about a
// narrowing conversion?!? Doing this by hand...
constexpr uint64_t ToNetworkU64(uint64_t in) {
  return ((in & UINT64_C(0xFF00000000000000)) >> 56) |
         ((in & UINT64_C(0x00FF000000000000)) >> 40) |
         ((in & UINT64_C(0x0000FF0000000000)) >> 24) |
         ((in & UINT64_C(0x000000FF00000000)) >> 8) |
         ((in & UINT64_C(0x00000000FF000000)) << 8) |
         ((in & UINT64_C(0x0000000000FF0000)) << 24) |
         ((in & UINT64_C(0x000000000000FF00)) << 40) |
         ((in & UINT64_C(0x00000000000000FF)) << 56);
}
constexpr int64_t ToNetwork64(int64_t in) {
  return static_cast<int64_t>(ToNetworkU64(static_cast<uint64_t>(in)));
}
constexpr uint32_t ToNetworkU32(uint32_t in) {
  return ((in & UINT32_C(0xFF000000)) >> 24) |
         ((in & UINT32_C(0x00FF0000)) >> 8) |
         ((in & UINT32_C(0x0000FF00)) << 8) |
         ((in & UINT32_C(0x000000FF)) << 24);
}
constexpr int32_t ToNetwork32(int32_t in) {
  return static_cast<int32_t>(ToNetworkU32(static_cast<uint32_t>(in)));
}
constexpr uint16_t ToNetworkU16(uint16_t in) {
  return ((in & UINT32_C(0xFF00)) >> 8) | ((in & UINT32_C(0x00FF)) << 8);
}
constexpr int16_t ToNetwork16(int16_t in) {
  return static_cast<int16_t>(ToNetworkU16(static_cast<uint16_t>(in)));
}
#elif __BYTE_ORDER == __BIG_ENDIAN
constexpr uint64_t ToNetworkU64(uint64_t in) { return in; }
constexpr int64_t ToNetwork64(int64_t in) { return in; }
constexpr uint32_t ToNetworkU32(uint32_t in) { return in; }
constexpr int32_t ToNetwork32(int32_t in) { return in; }
constexpr uint16_t ToNetworkU16(uint16_t in) { return in; }
constexpr int16_t ToNetwork16(int16_t in) { return in; }
#else
#error Unknown byte order.
#endif

// Varint encoding, as in
// https://developers.google.com/protocol-buffers/docs/encoding#varints

inline void AppendVar32(uint32_t in, std::string *out) {
  if (in < UINT32_C(1) << 7) {
    out->push_back(static_cast<char>(in));
  } else {
    internal::AppendVar32Slow(in, out);
  }
}

// Decode the first varint from |in|, saving it to |out| and advancing |in|.
// Returns error if |in| does not hold a complete varint or on integer overflow.
inline bool DecodeVar32(re2::StringPiece *in, uint32_t *out,
                        std::string *error_message) {
  if (in->size() == 0) {
    *error_message = "buffer underrun";
    return false;
  }
  auto first_byte = static_cast<uint8_t>(*in->data());
  if (first_byte < 0x80) {
    in->remove_prefix(1);
    *out = first_byte;
    return true;
  } else {
    return internal::DecodeVar32Slow(in, out, error_message);
  }
}

// Zigzag encoding for signed integers, as in
// https://developers.google.com/protocol-buffers/docs/encoding#types
// Use the low bit to indicate signedness (1 = negative, 0 = non-negative).
inline uint32_t Zigzag32(int32_t in) {
  return static_cast<uint32_t>(in << 1) ^ (in >> 31);
}

inline int32_t Unzigzag32(uint32_t in) {
  return (in >> 1) ^ -static_cast<int32_t>(in & 1);
}

inline void AppendU16(uint16_t in, std::string *out) {
  uint16_t net = ToNetworkU16(in);
  out->append(reinterpret_cast<const char *>(&net), sizeof(uint16_t));
}

inline void AppendU32(uint32_t in, std::string *out) {
  uint32_t net = ToNetworkU32(in);
  out->append(reinterpret_cast<const char *>(&net), sizeof(uint32_t));
}

inline void Append32(int32_t in, std::string *out) {
  int32_t net = ToNetwork32(in);
  out->append(reinterpret_cast<const char *>(&net), sizeof(int32_t));
}

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_CODING_H
