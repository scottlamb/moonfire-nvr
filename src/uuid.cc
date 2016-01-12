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
// uuid.cc: implementation of uuid.h interface.

#include "uuid.h"

namespace moonfire_nvr {

namespace {

const size_t kTextFormatLength =
    sizeof("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx") - 1;

}  // namespace

bool Uuid::ParseText(re2::StringPiece input) {
  if (input.size() != kTextFormatLength) {
    return false;
  }
  char tmp[kTextFormatLength + 1];
  memcpy(tmp, input.data(), kTextFormatLength);
  tmp[kTextFormatLength] = 0;
  return uuid_parse(tmp, me_) == 0;
}

bool Uuid::ParseBinary(re2::StringPiece input) {
  if (input.size() != sizeof(uuid_t)) {
    return false;
  }
  memcpy(me_, input.data(), sizeof(uuid_t));
  return true;
}

std::string Uuid::UnparseText() const {
  char tmp[kTextFormatLength + 1];
  uuid_unparse_lower(me_, tmp);
  return tmp;
}

re2::StringPiece Uuid::binary_view() const {
  return re2::StringPiece(reinterpret_cast<const char *>(me_), sizeof(me_));
}

bool Uuid::operator==(const Uuid &other) const {
  return uuid_compare(me_, other.me_) == 0;
}

bool Uuid::operator<(const Uuid &other) const {
  return uuid_compare(me_, other.me_) < 0;
}

class RealUuidGenerator : public UuidGenerator {
 public:
  Uuid Generate() final {
    Uuid out;
    uuid_generate(out.me_);
    return out;
  }
};

UuidGenerator *GetRealUuidGenerator() {
  static RealUuidGenerator *gen = new RealUuidGenerator;  // never freed.
  return gen;
}

}  // namespace moonfire_nvr
