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
// sqlite.cc: implementation of the sqlite.h interface.

#include "sqlite.h"

#include <glog/logging.h>

#include "string.h"

namespace moonfire_nvr {

bool Statement::BindBlob(int param, re2::StringPiece value,
                         std::string *error_message) {
  int err = sqlite3_bind_blob64(me_, param, value.data(), value.size(),
                                SQLITE_TRANSIENT);
  if (err != SQLITE_OK) {
    *error_message = sqlite3_errstr(err);
    return false;
  }
  return true;
}

bool Statement::BindInt64(int param, int64_t value,
                          std::string *error_message) {
  int err = sqlite3_bind_int64(me_, param, value);
  if (err != SQLITE_OK) {
    *error_message = sqlite3_errstr(err);
    return false;
  }
  return true;
}

bool Statement::BindText(int param, re2::StringPiece value,
                         std::string *error_message) {
  int err = sqlite3_bind_text64(me_, param, value.data(), value.size(),
                                SQLITE_TRANSIENT, SQLITE_UTF8);
  if (err != SQLITE_OK) {
    *error_message = sqlite3_errstr(err);
    return false;
  }
  return true;
}

re2::StringPiece Statement::ColumnBlob(int col) {
  // Order matters: call _blob first, then _bytes.
  const void *data = sqlite3_column_blob(me_, col);
  size_t len = sqlite3_column_bytes(me_, col);
  return re2::StringPiece(reinterpret_cast<const char *>(data), len);
}

int64_t Statement::ColumnInt64(int col) {
  return sqlite3_column_int64(me_, col);
}

re2::StringPiece Statement::ColumnText(int col) {
  // Order matters: call _text first, then _bytes.
  const unsigned char *data = sqlite3_column_text(me_, col);
  size_t len = sqlite3_column_bytes(me_, col);
  return re2::StringPiece(reinterpret_cast<const char *>(data), len);
}

int Statement::Step() { return sqlite3_step(me_); }

Database::~Database() {
  int err = sqlite3_close(me_);
  CHECK_EQ(SQLITE_OK, err) << "sqlite3_close: " << sqlite3_errstr(err);
}

bool Database::Open(const char *filename, int flags,
                    std::string *error_message) {
  int err = sqlite3_open_v2(filename, &me_, flags, nullptr);
  if (err != SQLITE_OK) {
    *error_message = sqlite3_errstr(err);
    return false;
  }
  return true;
}

std::unique_ptr<Statement> Database::Prepare(re2::StringPiece sql, size_t *used,
                                             std::string *error_message) {
  std::unique_ptr<Statement> statement(new Statement);
  const char *tail;
  int err =
      sqlite3_prepare_v2(me_, sql.data(), sql.size(), &statement->me_, &tail);
  if (err != SQLITE_OK) {
    *error_message = sqlite3_errstr(err);
    statement.release();
  }
  if (used != nullptr) {
    *used = tail - sql.data();
  }
  if (statement->me_ == nullptr) {
    error_message->clear();
    statement.release();
  }
  return statement;
}

bool RunStatements(Database *db, re2::StringPiece stmts,
                   std::string *error_message) {
  while (true) {
    size_t used;
    auto stmt = db->Prepare(stmts, &used, error_message);
    if (stmt == nullptr) {
      // Statement didn't parse. If |error_message| is empty, there are just no
      // more statements. Otherwise this is due to an error. Either way, return.
      return error_message->empty();
    }
    VLOG(1) << "Running statement:\n" << stmts.substr(0, used).as_string();
    stmts.remove_prefix(used);
    int ret = stmt->Step();
    if (ret != SQLITE_DONE) {
      *error_message =
          StrCat("Unexpected status \"", sqlite3_errstr(ret),
                 "\" from statement: \"", stmts.substr(0, used), "\"");
      return false;
    }
  }
}

}  // namespace moonfire_nvr
