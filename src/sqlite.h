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

#include <functional>
#include <mutex>
#include <string>

#include <glog/logging.h>
#include <re2/stringpiece.h>
#include <sqlite3.h>

#include "common.h"

namespace moonfire_nvr {

// Prepared statement. Movable, not copyable.
// The caller can obtain a Statement via Database::Prepare
// and use one via DatabaseContext::Borrow.
class Statement {
 public:
  Statement() {}
  Statement(Statement &&);
  void operator=(Statement &&);
  ~Statement();

  bool valid() const { return me_ != nullptr; }

 private:
  friend class Database;
  friend class DatabaseContext;
  friend class RunningStatement;

  Statement(const Statement &) = delete;
  void operator=(const Statement &) = delete;

  void Clear();

  sqlite3_stmt *me_ = nullptr;  // owned.
  bool borrowed_ = false;
};

class Database {
 public:
  Database() {}
  Database(const Database &) = delete;
  Database &operator=(const Database &) = delete;

  // PRE: all DatabaseContext and Statement objects have been deleted.
  ~Database();

  // Open the database and do initial setup.
  //
  // Foreign keys will always be enabled via "pragma foreign_keys = true;".
  //
  // extended result codes will always be enabled via
  // sqlite3_extended_result_codes.
  bool Open(const char *filename, int flags, std::string *error_message);

  // Prepare a statement. Thread-safe.
  //
  // |used|, if non-null, will be updated with the number of bytes used from
  // |sql| on success. (Only the first statement is parsed.)
  //
  // Returns a statement, which may or may not be valid().
  // |error_message| will be empty if there is simply no statement to parse.
  Statement Prepare(re2::StringPiece sql, size_t *used,
                    std::string *error_message);

 private:
  friend class DatabaseContext;
  friend class RunningStatement;
  sqlite3 *me_ = nullptr;
  Statement begin_transaction_;
  Statement commit_transaction_;
  Statement rollback_transaction_;

  std::mutex ctx_mu_;  // used by DatabaseContext.
};

// A running statement; get via DatabaseContext::Borrow or
// DatabaseContext::UseOnce. Example uses:
//
// {
//   DatabaseContext ctx(&db);
//   auto run = ctx.UseOnce("insert into table (column) values (:column)");
//   run.BindText(":column", "value");
//   if (run.Step() != SQLITE_DONE) {
//     LOG(ERROR) << "Operation failed: " << run.error_message();
//     return;
//   }
//   int64_t rowid = ctx.last_insert_rowid();
// }
//
// Statement select_stmt = db.Prepare(
//     "select rowid from table;", nullptr, &error_message);
// ...
// {
//   auto run = ctx.Borrow(&select_stmt);
//   while (run.Step() == SQLITE_ROW) {
//     int64_t rowid = op.ColumnInt64(0);
//     ...
//   }
//   if (run.status() != SQLITE_DONE) {
//     LOG(ERROR) << "Operation failed: " << run.error_message();
//   }
// }
class RunningStatement {
 public:
  RunningStatement(RunningStatement &&) = default;

  // Reset/unbind/return the statement for the next use (in the case of
  // Borrow) or delete it (in the case of UseOnce).
  ~RunningStatement();

  // Bind a value to a parameter. Call before the first Step.
  // |param| is indexed from 1 (unlike columns!).
  //
  // StringPiece |value|s will be copied; they do not need to outlive the
  // Bind{Blob,Text} call. Text values are assumed to be UTF-8.
  //
  // Errors are deferred until Step() for simplicity of caller code.
  void BindBlob(int param, re2::StringPiece value);
  void BindBlob(const char *param, re2::StringPiece value);
  void BindText(int param, re2::StringPiece value);
  void BindText(const char *param, re2::StringPiece value);
  void BindInt64(int param, int64_t value);
  void BindInt64(const char *param, int64_t value);

  // Advance the statement, returning SQLITE_ROW, SQLITE_DONE, or an error.
  // Note that this may return a "deferred error" if UseOnce failed to parse
  // the SQL or if a bind failed.
  int Step();

  // Convenience function; re-return the last status from Step().
  int status() { return status_; }

  // Return a stringified version of the last status.
  // This may have more information than sqlite3_errstr(status()),
  // in the case of "deferred errors".
  std::string error_message() { return error_message_; }

  // Column accessors, to be called after Step() returns SQLITE_ROW.
  // Columns are indexed from 0 (unlike bind parameters!).
  // StringPiece values are valid only until a type conversion, the following
  // NextRow() call, or destruction of the RunningStatement, whichever
  // happens first.
  //
  // Note there is no useful way to report error here. In particular, the
  // underlying SQLite functions return a default value on error, which can't
  // be distinguished from a legitimate value. The error code is set on the
  // database, but it's not guaranteed to *not* be set if there's no error.

  // Return the type of a given column; if SQLITE_NULL, the value is null.
  // As noted in sqlite3_column_type() documentation, this is only meaningful
  // if other Column* calls have not forced a type conversion.
  int ColumnType(int col);

  re2::StringPiece ColumnBlob(int col);
  int64_t ColumnInt64(int col);
  re2::StringPiece ColumnText(int col);

 private:
  friend class DatabaseContext;
  RunningStatement(Statement *stmt, const std::string &deferred_error,
                   bool own_statement);
  RunningStatement(const RunningStatement &) = delete;
  void operator=(const RunningStatement &) = delete;

  Statement *statement_ = nullptr;  // maybe owned; see owns_statement_.
  std::string error_message_;
  int status_ = SQLITE_OK;
  bool owns_statement_ = false;
};

// A scoped database lock and transaction manager.
//
// Moonfire NVR does all SQLite operations under a lock, to avoid SQLITE_BUSY
// and so that calls such as sqlite3_last_insert_rowid return useful values.
// This class implicitly acquires the lock on entry / releases it on exit.
// In the future, it may have instrumentation to track slow operations.
class DatabaseContext {
 public:
  // Acquire a lock on |db|, which must already be opened.
  explicit DatabaseContext(Database *db);
  DatabaseContext(const DatabaseContext &) = delete;
  void operator=(const DatabaseContext &) = delete;

  // Release the lock and, if an explicit transaction is active, roll it
  // back with a logged warning.
  ~DatabaseContext();

  // Begin a transaction, or return false and fill |error_message|.
  // If successful, the caller should explicitly call CommitTransaction or
  // RollbackTransaction before the DatabaseContext goes out of scope.
  bool BeginTransaction(std::string *error_message);

  // Commit the transaction, or return false and fill |error_message|.
  bool CommitTransaction(std::string *error_message);

  // Roll back the transaction, logging error on failure.
  // The error code is not returned; there's nothing useful the caller can do.
  void RollbackTransaction();

  // Borrow a prepared statement to run.
  // |statement| should outlive the RunningStatement. It can't be borrowed
  // twice simultaneously, but two similar statements can be run side-by-side
  // (in the same context).
  RunningStatement Borrow(Statement *statement);

  // Use the given |sql| once.
  // Note that parse errors are "deferred" until RunningStatement::Step().
  RunningStatement UseOnce(re2::StringPiece sql);

  // Return the number of changes for the last DML statement (insert, update, or
  // delete), as with sqlite3_changes.
  int64_t changes() { return sqlite3_changes(db_->me_); }

  // Return the last rowid inserted into a table that does not specify "WITHOUT
  // ROWID", as with sqlite3_last_insert_rowid.
  int64_t last_insert_rowid() { return sqlite3_last_insert_rowid(db_->me_); }

  Database *db() { return db_; }

 private:
  Database *db_;
  std::lock_guard<std::mutex> lock_;
  bool transaction_open_ = false;
};

// Convenience routines below.

// Run through all the statements in |stmts|.
// Return error if any do not parse or return something other than SQLITE_DONE
// when stepped. (SQLITE_ROW returns are skipped over, though. This is useful
// for "pragma journal_mode = wal;" which returns a row.)
bool RunStatements(DatabaseContext *ctx, re2::StringPiece stmts,
                   std::string *error_message);

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_SQLITE_H
