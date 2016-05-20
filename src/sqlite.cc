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

#include <mutex>

#include <glog/logging.h>

#include "string.h"

namespace moonfire_nvr {

namespace {

void LogCallback(void *, int err_code, const char *msg) {
  LOG(ERROR) << "(" << err_code << ") " << msg;
}

void GlobalSetup() {
  VLOG(1) << "Installing sqlite3 log callback";
  sqlite3_config(SQLITE_CONFIG_LOG, &LogCallback, nullptr);
}

std::once_flag global_setup;

}  // namespace

Statement::Statement(Statement &&other) { *this = std::move(other); }

void Statement::operator=(Statement &&other) {
  Clear();
  memcpy(this, &other, sizeof(Statement));
  other.me_ = nullptr;
  other.borrowed_ = false;
}

Statement::~Statement() { Clear(); }

void Statement::Clear() {
  CHECK(!borrowed_) << "can't delete statement while still borrowed!";
  sqlite3_finalize(me_);
}

DatabaseContext::DatabaseContext(Database *db) : db_(db), lock_(db->ctx_mu_) {}

DatabaseContext::~DatabaseContext() {
  if (transaction_open_) {
    LOG(WARNING) << this << ": transaction left open! closing in destructor.";
    RollbackTransaction();
  }
}

bool DatabaseContext::BeginTransaction(std::string *error_message) {
  if (transaction_open_) {
    *error_message = "transaction already open!";
    return false;
  }
  sqlite3_step(db_->begin_transaction_.me_);
  int ret = sqlite3_reset(db_->begin_transaction_.me_);
  if (ret != SQLITE_OK) {
    *error_message =
        StrCat("begin transaction: ", sqlite3_errstr(ret), " (", ret, ")");
    return false;
  }
  transaction_open_ = true;
  return true;
}

bool DatabaseContext::CommitTransaction(std::string *error_message) {
  if (!transaction_open_) {
    *error_message = "transaction not open!";
    return false;
  }
  sqlite3_step(db_->commit_transaction_.me_);
  int ret = sqlite3_reset(db_->commit_transaction_.me_);
  if (ret != SQLITE_OK) {
    *error_message =
        StrCat("commit transaction: ", sqlite3_errstr(ret), " (", ret, ")");
    return false;
  }
  transaction_open_ = false;
  return true;
}

void DatabaseContext::RollbackTransaction() {
  if (!transaction_open_) {
    LOG(WARNING) << this << ": rollback failed: transaction not open!";
    return;
  }
  sqlite3_step(db_->rollback_transaction_.me_);
  int ret = sqlite3_reset(db_->rollback_transaction_.me_);
  if (ret != SQLITE_OK) {
    LOG(WARNING) << this << ": rollback failed: " << sqlite3_errstr(ret) << " ("
                 << ret << ")";
    return;
  }
  transaction_open_ = false;
}

RunningStatement DatabaseContext::Borrow(Statement *statement) {
  return RunningStatement(statement, std::string(), false);
}

RunningStatement DatabaseContext::UseOnce(re2::StringPiece sql) {
  std::string error_message;
  auto *statement = new Statement(db_->Prepare(sql, nullptr, &error_message));
  return RunningStatement(statement, error_message, true);
}

RunningStatement::RunningStatement(Statement *statement,
                                   const std::string &deferred_error,
                                   bool owns_statement)
    : error_message_(deferred_error), owns_statement_(owns_statement) {
  if (statement != nullptr && statement->valid()) {
    CHECK(!statement->borrowed_) << "Statement already borrowed!";
    statement->borrowed_ = true;
    statement_ = statement;
  } else if (error_message_.empty()) {
    error_message_ = "invalid statement";
  }

  if (!error_message_.empty()) {
    status_ = SQLITE_MISUSE;
  }
}

RunningStatement::RunningStatement(RunningStatement &&o) {
  statement_ = o.statement_;
  status_ = o.status_;
  owns_statement_ = o.owns_statement_;
  o.statement_ = nullptr;
}

RunningStatement::~RunningStatement() {
  if (statement_ != nullptr) {
    CHECK(statement_->borrowed_) << "Statement no longer borrowed!";
    sqlite3_clear_bindings(statement_->me_);
    sqlite3_reset(statement_->me_);
    statement_->borrowed_ = false;
    if (owns_statement_) {
      delete statement_;
    }
  }
}

void RunningStatement::BindBlob(int param, re2::StringPiece value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  status_ = sqlite3_bind_blob64(statement_->me_, param, value.data(),
                                value.size(), SQLITE_TRANSIENT);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", param, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

void RunningStatement::BindBlob(const char *name, re2::StringPiece value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  int param = sqlite3_bind_parameter_index(statement_->me_, name);
  if (param == 0) {
    status_ = SQLITE_MISUSE;
    error_message_ = StrCat("Unable to bind parameter ", name, ": not found.");
    return;
  }
  status_ = sqlite3_bind_blob64(statement_->me_, param, value.data(),
                                value.size(), SQLITE_TRANSIENT);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", name, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

void RunningStatement::BindInt64(int param, int64_t value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  status_ = sqlite3_bind_int64(statement_->me_, param, value);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", param, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

void RunningStatement::BindInt64(const char *name, int64_t value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  int param = sqlite3_bind_parameter_index(statement_->me_, name);
  if (param == 0) {
    status_ = SQLITE_MISUSE;
    error_message_ = StrCat("Unable to bind parameter ", name, ": not found.");
    return;
  }
  status_ = sqlite3_bind_int64(statement_->me_, param, value);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", name, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

void RunningStatement::BindText(int param, re2::StringPiece value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  status_ = sqlite3_bind_text64(statement_->me_, param, value.data(),
                                value.size(), SQLITE_TRANSIENT, SQLITE_UTF8);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", param, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

void RunningStatement::BindText(const char *name, re2::StringPiece value) {
  if (status_ != SQLITE_OK) {
    return;
  }
  int param = sqlite3_bind_parameter_index(statement_->me_, name);
  if (param == 0) {
    error_message_ = StrCat("Unable to bind parameter ", name, ": not found.");
    return;
  }
  status_ = sqlite3_bind_text64(statement_->me_, param, value.data(),
                                value.size(), SQLITE_TRANSIENT, SQLITE_UTF8);
  if (status_ != SQLITE_OK) {
    error_message_ = StrCat("Unable to bind parameter ", name, ": ",
                            sqlite3_errstr(status_), " (", status_, ")");
  }
}

int RunningStatement::Step() {
  if (status_ != SQLITE_OK && status_ != SQLITE_ROW) {
    return status_;
  }
  status_ = sqlite3_step(statement_->me_);
  error_message_ =
      StrCat("step: ", sqlite3_errstr(status_), " (", status_, ")");
  return status_;
}

int RunningStatement::ColumnType(int col) {
  return sqlite3_column_type(statement_->me_, col);
}

re2::StringPiece RunningStatement::ColumnBlob(int col) {
  // Order matters: call _blob first, then _bytes.
  const void *data = sqlite3_column_blob(statement_->me_, col);
  size_t len = sqlite3_column_bytes(statement_->me_, col);
  return re2::StringPiece(reinterpret_cast<const char *>(data), len);
}

int64_t RunningStatement::ColumnInt64(int col) {
  return sqlite3_column_int64(statement_->me_, col);
}

re2::StringPiece RunningStatement::ColumnText(int col) {
  // Order matters: call _text first, then _bytes.
  const unsigned char *data = sqlite3_column_text(statement_->me_, col);
  size_t len = sqlite3_column_bytes(statement_->me_, col);
  return re2::StringPiece(reinterpret_cast<const char *>(data), len);
}

Database::~Database() {
  begin_transaction_ = Statement();
  commit_transaction_ = Statement();
  rollback_transaction_ = Statement();
  int err = sqlite3_close(me_);
  CHECK_EQ(SQLITE_OK, err) << "sqlite3_close: " << sqlite3_errstr(err);
}

bool Database::Open(const char *filename, int flags,
                    std::string *error_message) {
  std::call_once(global_setup, &GlobalSetup);
  int ret = sqlite3_open_v2(filename, &me_, flags, nullptr);
  if (ret != SQLITE_OK) {
    *error_message =
        StrCat("open ", filename, ": ", sqlite3_errstr(ret), " (", ret, ")");
    return false;
  }

  ret = sqlite3_extended_result_codes(me_, 1);
  if (ret != SQLITE_OK) {
    sqlite3_close(me_);
    me_ = nullptr;
    *error_message = StrCat("while enabling extended result codes: ",
                            sqlite3_errstr(ret), " (", ret, ")");
    return false;
  }

  Statement pragma_foreignkeys;
  struct StatementToInitialize {
    Statement *p;
    re2::StringPiece sql;
  };
  StatementToInitialize stmts[] = {
      {&begin_transaction_, "begin transaction;"},
      {&commit_transaction_, "commit transaction;"},
      {&rollback_transaction_, "rollback transaction;"},
      {&pragma_foreignkeys, "pragma foreign_keys = true;"}};

  for (const auto &stmt : stmts) {
    *stmt.p = Prepare(stmt.sql, nullptr, error_message);
    if (!stmt.p->valid()) {
      sqlite3_close(me_);
      me_ = nullptr;
      *error_message = StrCat("while preparing SQL for \"", stmt.sql, "\": ",
                              *error_message);
      return false;
    }
  }

  ret = sqlite3_step(pragma_foreignkeys.me_);
  sqlite3_reset(pragma_foreignkeys.me_);
  if (ret != SQLITE_DONE) {
    sqlite3_close(me_);
    me_ = nullptr;
    *error_message = StrCat("while enabling foreign keys: ",
                            sqlite3_errstr(ret), " (", ret, ")");
    return false;
  }

  return true;
}

Statement Database::Prepare(re2::StringPiece sql, size_t *used,
                            std::string *error_message) {
  Statement statement;
  const char *tail;
  int err =
      sqlite3_prepare_v2(me_, sql.data(), sql.size(), &statement.me_, &tail);
  if (err != SQLITE_OK) {
    *error_message = StrCat("prepare: ", sqlite3_errstr(err), " (", err, ")");
    return statement;
  }
  if (used != nullptr) {
    *used = tail - sql.data();
  }
  if (statement.me_ == nullptr) {
    error_message->clear();
  }
  return statement;
}

bool RunStatements(DatabaseContext *ctx, re2::StringPiece stmts,
                   std::string *error_message) {
  while (true) {
    size_t used = 0;
    auto stmt = ctx->db()->Prepare(stmts, &used, error_message);
    if (!stmt.valid()) {
      // Statement didn't parse. If |error_message| is empty, there are just no
      // more statements. Otherwise this is due to an error. Either way, return.
      return error_message->empty();
    }
    VLOG(1) << "Running statement:\n" << stmts.substr(0, used).as_string();
    int64_t rows = 0;
    auto run = ctx->Borrow(&stmt);
    while (run.Step() == SQLITE_ROW) {
      ++rows;
    }
    if (rows > 0) {
      VLOG(1) << "Statement returned " << rows << " row(s).";
    }
    if (run.status() != SQLITE_DONE) {
      VLOG(1) << "Statement failed with " << run.status() << ": "
              << run.error_message();
      *error_message =
          StrCat("Unexpected error ", run.error_message(),
                 " from statement: \"", stmts.substr(0, used), "\"");
      return false;
    }
    VLOG(1) << "Statement succeeded.";
    stmts.remove_prefix(used);
  }
}

}  // namespace moonfire_nvr
