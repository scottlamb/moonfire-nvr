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
// string.cc: See string.h.

#include "string.h"

#include <string.h>

#include <glog/logging.h>

namespace moonfire_nvr {

namespace {

char HexDigit(unsigned int i) {
  static char kHexadigits[] = "0123456789abcdef";
  return (i < 16) ? kHexadigits[i] : 'x';
}

}  // namespace

namespace internal {

StrCatPiece::StrCatPiece(uint64_t p) {
  if (p == 0) {
    piece_ = "0";
  } else {
    size_t i = sizeof(buf_);
    while (p != 0) {
      buf_[--i] = '0' + (p % 10);
      p /= 10;
    }
    piece_.set(buf_ + i, sizeof(buf_) - i);
  }
}

StrCatPiece::StrCatPiece(int64_t p) {
  if (p == 0) {
    piece_ = "0";
  } else {
    bool negative = p < 0;
    size_t i = sizeof(buf_);
    while (p != 0) {
      buf_[--i] = '0' + std::abs(p % 10);
      p /= 10;
    }
    if (negative) {
      buf_[--i] = '-';
    }
    piece_.set(buf_ + i, sizeof(buf_) - i);
  }
}

}  // namespace internal

bool IsWord(const std::string &str) {
  for (char c : str) {
    if (!(('0' <= c && c <= '9') || ('A' <= c && c <= 'Z') ||
          ('a' <= c && c <= 'z') || c == '_')) {
      return false;
    }
  }
  return true;
}

std::string EscapeHtml(const std::string &input) {
  std::string output;
  output.reserve(input.size());
  for (char c : input) {
    switch (c) {
      case '&':
        output.append("&amp;");
        break;
      case '<':
        output.append("&lt;");
        break;
      case '>':
        output.append("&gt;");
        break;
      default:
        output.push_back(c);
    }
  }
  return output;
}

std::string ToHex(re2::StringPiece in) {
  std::string out;
  out.reserve(in.size() * 3 + 1);
  for (int i = 0; i < in.size(); ++i) {
    if (i > 0) out.push_back(' ');
    uint8_t byte = in[i];
    out.push_back(HexDigit(byte >> 4));
    out.push_back(HexDigit(byte & 0x0F));
  }
  return out;
}

bool strto64(const char *str, int base, const char **endptr, int64_t *value) {
  static_assert(sizeof(int64_t) == sizeof(long long int),
                "unknown memory model");
  errno = 0;
  *value = ::strtoll(str, const_cast<char **>(endptr), base);
  return *endptr != str && errno == 0;
}

}  // namespace moonfire_nvr
