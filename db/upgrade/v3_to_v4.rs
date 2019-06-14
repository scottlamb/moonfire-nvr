// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2018 Scott Lamb <slamb@slamb.org>
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

/// Upgrades a version 3 schema to a version 4 schema.

use failure::Error;

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 4 was the latest.
    tx.execute_batch(r#"
        create table signal (
          id integer primary key,
          source_uuid blob not null check (length(source_uuid) = 16),
          type_uuid blob not null check (length(type_uuid) = 16),
          short_name not null,
          unique (source_uuid, type_uuid)
        );

        create table signal_type_enum (
          type_uuid blob not null check (length(type_uuid) = 16),
          value integer not null check (value > 0 and value < 16),
          name text not null,
          motion int not null check (motion in (0, 1)) default 0,
          color text
        );

        create table signal_camera (
          signal_id integer references signal (id),
          camera_id integer references camera (id),
          type integer not null,
          primary key (signal_id, camera_id)
        ) without rowid;

        create table signal_state (
          time_90k integer primary key,
          changes blob
        );
    "#)?;
    Ok(())
}
