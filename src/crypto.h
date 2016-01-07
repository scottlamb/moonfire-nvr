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
// crypto.h: cryptographic functions.

#ifndef MOONFIRE_NVR_CRYPTO_H
#define MOONFIRE_NVR_CRYPTO_H

#include <memory>

#include <openssl/evp.h>
#include <re2/stringpiece.h>

namespace moonfire_nvr {

class Digest {
 public:
  static std::unique_ptr<Digest> SHA1();
  ~Digest();

  // PRE: Finalize() has not been called.
  void Update(re2::StringPiece data);

  // PRE: Finalize() has not been called.
  std::string Finalize();

 private:
  Digest();
  Digest(const Digest &) = delete;
  void operator=(const Digest &) = delete;
  EVP_MD_CTX *ctx_ = nullptr;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_CRYPTO_H
