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
// crypto.cc: see crypto.h.

#include "crypto.h"

#include <glog/logging.h>

namespace moonfire_nvr {

std::unique_ptr<Digest> Digest::SHA1() {
  std::unique_ptr<Digest> d(new Digest);
  CHECK_EQ(1, EVP_DigestInit_ex(d->ctx_, EVP_sha1(), nullptr));
  return d;
}

Digest::Digest() { ctx_ = CHECK_NOTNULL(EVP_MD_CTX_create()); }

Digest::~Digest() { EVP_MD_CTX_destroy(ctx_); }

void Digest::Update(re2::StringPiece data) {
  CHECK_EQ(1, EVP_DigestUpdate(ctx_, data.data(), data.size()));
}

std::string Digest::Finalize() {
  std::string out;
  out.resize(EVP_MD_CTX_size(ctx_));
  auto *p = reinterpret_cast<unsigned char *>(&out[0]);
  CHECK_EQ(1, EVP_DigestFinal_ex(ctx_, p, nullptr));
  return out;
}

}  // namespace moonfire_nvr
