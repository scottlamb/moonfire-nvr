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
// string.h: convenience methods for dealing with strings.

#ifndef MOONFIRE_NVR_STRING_H
#define MOONFIRE_NVR_STRING_H

#include <string>

#include <re2/stringpiece.h>

namespace moonfire_nvr {

namespace internal {

// Only used within StrCat() and Join().
// Note implicit constructor, which is necessary to avoid a copy,
// though it could be wrapped in another type.
// http://stackoverflow.com/questions/34112755/can-i-avoid-a-c11-move-when-initializing-an-array/34113744
class StrCatPiece {
 public:
  StrCatPiece(uint64_t p);
  StrCatPiece(int64_t p);
  StrCatPiece(uint32_t p) : StrCatPiece(static_cast<uint64_t>(p)) {}
  StrCatPiece(int32_t p) : StrCatPiece(static_cast<int64_t>(p)) {}

#ifndef __LP64__  // if sizeof(long) == sizeof(int32_t)
  // Need to resolve ambiguity.
  StrCatPiece(long p) : StrCatPiece(static_cast<int32_t>(p)) {}
  StrCatPiece(unsigned long p) : StrCatPiece(static_cast<uint32_t>(p)) {}
#endif

  StrCatPiece(re2::StringPiece p) : piece_(p) {}

  StrCatPiece(const StrCatPiece &) = delete;
  StrCatPiece &operator=(const StrCatPiece &) = delete;

  const char *data() const { return piece_.data(); }
  size_t size() const { return piece_.size(); }

 private:
  // Not allowed: ambiguous meaning.
  StrCatPiece(char);

  // |piece_| points either to within buf_ (numeric constructors) or to unowned
  // string data (StringPiece constructor).
  re2::StringPiece piece_;
  char buf_[20];  // length of maximum uint64 (no terminator needed).
};

}  // namespace internal

// Concatenate any number of strings, StringPieces, and numeric values into a
// single string.
template <typename... Types>
std::string StrCat(Types... args) {
  internal::StrCatPiece pieces[] = {{args}...};
  size_t size = 0;
  for (const auto &p : pieces) {
    size += p.size();
  }
  std::string out;
  out.reserve(size);
  for (const auto &p : pieces) {
    out.append(p.data(), p.size());
  }
  return out;
}

// Join any number of string fragments (of like type) together into a single
// string, with a separator.
template <typename Container>
std::string Join(const Container &pieces, re2::StringPiece separator) {
  std::string out;
  bool first = true;
  for (const auto &p : pieces) {
    if (!first) {
      out.append(separator.data(), separator.size());
    }
    first = false;
    internal::StrCatPiece piece(p);
    out.append(piece.data(), piece.size());
  }
  return out;
}

// Return true if every character in |str| is in [A-Za-z0-9_].
bool IsWord(const std::string &str);

// HTML-escape the given UTF-8-encoded string.
std::string EscapeHtml(const std::string &input);

// Wrapper around ::strtol that returns true iff valid and corrects
// constness.
bool strto64(const char *str, int base, const char **endptr, int64_t *value);

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_STRING_H
