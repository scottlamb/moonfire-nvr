// This file is part of Moonfire NVR, a security camera network video recorder.
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
// coding.cc: see coding.h.

#include "coding.h"

namespace moonfire_nvr {

namespace internal {

void AppendVar32Slow(uint32_t in, std::string *out) {
  while (true) {
    uint8_t next_byte = in & 0x7F;
    in >>= 7;
    if (in == 0) {
      out->push_back(next_byte);
      return;
    }
    out->push_back(next_byte | 0x80);
  }
}

bool DecodeVar32Slow(re2::StringPiece *in, uint32_t *out_p,
                     std::string *error_message) {
  auto p = reinterpret_cast<uint8_t const *>(in->data());
  auto end = p + in->size();
  uint32_t out = 0;
  int shift = 0;
  do {
    if (p == end) {
      *error_message = "buffer underrun";
      return false;
    }
    if (shift == 28 && *p & 0xf0) {
      *error_message = "integer overflow";
      return false;
    }
    out |= uint32_t(*p & 0x7f) << shift;
    shift += 7;
  } while ((*p++ & 0x80) != 0);
  *out_p = out;
  in->remove_prefix(reinterpret_cast<char const *>(p) - in->data());
  return true;
}

}  // namespace internal

}  // namespace moonfire_nvr
