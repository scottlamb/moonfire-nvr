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
// sqlite.h: a quick C++ wrapper interface around the SQLite3 C API.
// This provides RAII and takes advantage of some types like re2::StringPiece.
// It makes no attempt to hide how the underlying API works, so read
// alongside: <https://www.sqlite.org/capi3ref.html>

#ifndef MOONFIRE_NVR_SQLITE_H
#define MOONFIRE_NVR_SQLITE_H

#include <memory>

#include <re2/stringpiece.h>
#include <sqlite3.h>

namespace moonfire_nvr {

// Thread-compatible (not thread-safe).
class Statement {
 public:
  ~Statement() { sqlite3_finalize(me_); }

  // Bind a value (of various types).
  // |param| is 1-indexed.
  // In the case of BindBlob, |value| will be copied immediately using
  // SQLITE_TRANSIENT.
  // In the case of BindText, UTF-8 is assumed.
  bool BindBlob(int param, re2::StringPiece value, std::string *error_message);
  bool BindInt64(int param, int64_t value, std::string *error_message);
  bool BindText(int param, re2::StringPiece value, std::string *error_message);

  bool Reset(std::string *error_message);

  // Evaluate the statement.
  // Returns a SQLite3 result code or extended result code.
  // (Notably including SQLITE_ROW and SQLITE_DONE.)
  int Step();

  // Retrieve a value of various types.
  // StringPiece values are only valid until a type conversion or a following
  // call to Step(), Reset(), or Statement's destructor.
  // Note that these don't have any real way to report error.
  // In particular, on error a default value is returned, but that's a valid
  // value. The error code is set on the database, but it's not guaranteed to
  // not be set otherwise.
  re2::StringPiece ColumnBlob(int col);
  int64_t ColumnInt64(int col);
  re2::StringPiece ColumnText(int col);

 private:
  friend class Database;
  Statement() {}
  Statement(const Statement &) = delete;
  Statement &operator=(const Statement &) = delete;

  sqlite3_stmt *me_ = nullptr;
};

// Thread-safe; the database is used in the default SQLITE_CONFIG_SERIALIZED.
class Database {
 public:
  Database() {}
  Database(const Database &) = delete;
  Database &operator=(const Database &) = delete;

  // PRE: there are no unfinalized prepared statements or unfinished backup
  // objects.
  ~Database();

  bool Open(const char *filename, int flags, std::string *error_message);

  // Prepare a statement.
  //
  // |used|, if non-null, will be updated with the number of bytes used from
  // |sql| on success. (Only the first statement is parsed.)
  //
  // Returns the statement, or nullptr if there is no valid statement.
  // |error_message| will be empty if there is simply no statement to parse.
  std::unique_ptr<Statement> Prepare(re2::StringPiece sql, size_t *used,
                                     std::string *error_message);

  // Return the number of rows modified/inserted/deleted by the last DML
  // statement executed.
  int Changes() { return sqlite3_changes(me_); }

 private:
  sqlite3 *me_ = nullptr;
};

// Convenience routines below.

// Run through all the statements in |stmts|.
// Return error if any do not parse or return something other than SQLITE_DONE
// when stepped.
bool RunStatements(Database *db, re2::StringPiece stmts,
                   std::string *error_message);

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_SQLITE_H
