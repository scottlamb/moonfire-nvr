// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Comparison of actual and expected on-disk schema.
//! This is used as part of the `moonfire-nvr check` database integrity checking
//! and for tests of `moonfire-nvr upgrade`.

use failure::Error;
use prettydiff::diff_slice;
use rusqlite::params;
use std::fmt::Write;

#[derive(Debug, PartialEq)]
struct Column {
    cid: u32,
    name: String,
    type_: String,
    notnull: bool,
    dflt_value: rusqlite::types::Value,
    pk: u32,
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Index {
    seq: u32,
    name: String,
    unique: bool,
    origin: String,
    partial: bool,
}

impl std::fmt::Display for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct IndexColumn {
    seqno: u32,
    cid: u32,
    name: String,
}

impl std::fmt::Display for IndexColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Returns a sorted vec of table names in the given connection.
fn get_tables(c: &rusqlite::Connection) -> Result<Vec<String>, rusqlite::Error> {
    c.prepare(
        r#"
        select
            name
        from
            sqlite_master
        where
            type = 'table' and
            name not like 'sqlite_%'
        order by name
        "#,
    )?
    .query_map(params![], |r| r.get(0))?
    .collect()
}

/// Returns a vec of columns in the given table.
fn get_table_columns(
    c: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<Column>, rusqlite::Error> {
    // Note that placeholders aren't allowed for these pragmas. Just assume sane table names
    // (no escaping). "select * from pragma_..." syntax would be nicer but requires SQLite
    // 3.16.0 (2017-01-02). Ubuntu 16.04 Xenial (still used on Travis CI) has an older SQLite.
    c.prepare(&format!("pragma table_info(\"{}\")", table))?
        .query_map(params![], |r| {
            Ok(Column {
                cid: r.get(0)?,
                name: r.get(1)?,
                type_: r.get(2)?,
                notnull: r.get(3)?,
                dflt_value: r.get(4)?,
                pk: r.get(5)?,
            })
        })?
        .collect()
}

/// Returns a vec of indices associated with the given table.
fn get_indices(c: &rusqlite::Connection, table: &str) -> Result<Vec<Index>, rusqlite::Error> {
    // See note at get_tables_columns about placeholders.
    c.prepare(&format!("pragma index_list(\"{}\")", table))?
        .query_map(params![], |r| {
            Ok(Index {
                seq: r.get(0)?,
                name: r.get(1)?,
                unique: r.get(2)?,
                origin: r.get(3)?,
                partial: r.get(4)?,
            })
        })?
        .collect()
}

/// Returns a vec of all the columns in the given index.
fn get_index_columns(
    c: &rusqlite::Connection,
    index: &str,
) -> Result<Vec<IndexColumn>, rusqlite::Error> {
    // See note at get_tables_columns about placeholders.
    c.prepare(&format!("pragma index_info(\"{}\")", index))?
        .query_map(params![], |r| {
            Ok(IndexColumn {
                seqno: r.get(0)?,
                cid: r.get(1)?,
                name: r.get(2)?,
            })
        })?
        .collect()
}

pub fn get_diffs(
    n1: &str,
    c1: &rusqlite::Connection,
    n2: &str,
    c2: &rusqlite::Connection,
) -> Result<Option<String>, Error> {
    let mut diffs = String::new();

    // Compare table list.
    let tables1 = get_tables(c1)?;
    let tables2 = get_tables(c2)?;
    if tables1 != tables2 {
        write!(
            &mut diffs,
            "table list mismatch, {} vs {}:\n{}",
            n1,
            n2,
            diff_slice(&tables1, &tables2)
        )?;
    }

    // Compare columns and indices for each table.
    for t in &tables1 {
        let columns1 = get_table_columns(c1, &t)?;
        let columns2 = get_table_columns(c2, &t)?;
        if columns1 != columns2 {
            write!(
                &mut diffs,
                "table {:?} column, {} vs {}:\n{}",
                t,
                n1,
                n2,
                diff_slice(&columns1, &columns2)
            )?;
        }

        let mut indices1 = get_indices(c1, &t)?;
        let mut indices2 = get_indices(c2, &t)?;
        indices1.sort_by(|a, b| a.name.cmp(&b.name));
        indices2.sort_by(|a, b| a.name.cmp(&b.name));
        if indices1 != indices2 {
            write!(
                &mut diffs,
                "table {:?} indices, {} vs {}:\n{}",
                t,
                n1,
                n2,
                diff_slice(&indices1, &indices2)
            )?;
        }

        for i in &indices1 {
            let ic1 = get_index_columns(c1, &i.name)?;
            let ic2 = get_index_columns(c2, &i.name)?;
            if ic1 != ic2 {
                write!(
                    &mut diffs,
                    "table {:?} index {:?} columns {} vs {}:\n{}",
                    t,
                    i,
                    n1,
                    n2,
                    diff_slice(&ic1, &ic2)
                )?;
            }
        }
    }

    Ok(if diffs.is_empty() { None } else { Some(diffs) })
}
