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
// uuid.h: small wrapper around the C UUID library for generating/parsing
// RFC 4122 UUIDs.

#ifndef MOONFIRE_NVR_UUID_H
#define MOONFIRE_NVR_UUID_H

#include <gmock/gmock.h>
#include <re2/stringpiece.h>
#include <uuid/uuid.h>

namespace moonfire_nvr {

class Uuid {
 public:
  // Create a null uuid.
  Uuid() { uuid_clear(me_); }

  // Parse the text UUID. Returns success.
  bool ParseText(re2::StringPiece input);

  // Parse a binary UUID. In practice any 16-byte string is considered valid.
  bool ParseBinary(re2::StringPiece input);

  // Return a 36-byte lowercase text representation, such as
  // 1b4e28ba-2fa1-11d2-883f-0016d3cca427.
  std::string UnparseText() const;

  // Return a reference to the 16-byte binary form.
  // Invalidated by any change to the Uuid object.
  re2::StringPiece binary_view() const;

  bool operator==(const Uuid &) const;
  bool operator<(const Uuid &) const;

  bool is_null() const { return uuid_is_null(me_); }

 private:
  friend class RealUuidGenerator;
  uuid_t me_;
};

class UuidGenerator {
 public:
  virtual ~UuidGenerator() {}
  virtual Uuid Generate() = 0;
};

class MockUuidGenerator : public UuidGenerator {
 public:
  MOCK_METHOD0(Generate, Uuid());
};

UuidGenerator *GetRealUuidGenerator();

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_CODING_H
