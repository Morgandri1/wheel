//! Table nodes: a node as a keyspace, backed by a real sqlite table.
//!
//! Every table node owns one table `t_<node name>` with an implicit
//! `key TEXT PRIMARY KEY` plus the columns the user configured (§3). Agents
//! address rows as `<table>/<row>`, and `wheel query` opens a read-only door
//! onto that one table and nothing else.
//!
//! Two things here are load-bearing for safety. Identifiers are interpolated
//! into SQL because sqlite cannot bind them — which is only safe because
//! `NodeName` and `Ident` restrict the charset to `[a-z0-9_]` and this module
//! refuses anything that did not come through them. And user SQL never touches
//! the engine's own connection: [`query`] opens a separate READ-ONLY connection
//! with an authorizer that allows reading exactly one table.

use std::{path::Path, time::Duration, time::Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    params_from_iter,
    types::{Value as SqlValue, ValueRef},
    Connection, OpenFlags, OptionalExtension,
};
use serde_json::{Map, Value};
use wheel_core::{ColumnType, NodeName, TableConfig};

/// The implicit primary key column every table node has (§3).
pub const KEY_COLUMN: &str = "key";

/// Read ceiling for any single call (§ "Read ceilings").
pub const MAX_ROWS: usize = 10_000;

/// How long a user's `wheel query` may run before it is aborted.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to check the clock while a query runs, in VM steps.
const PROGRESS_STEPS: i32 = 1_000;

/// Ceiling on any single string or blob a query may produce.
///
/// Without it `SELECT randomblob(1000000000)` asks sqlite for a gigabyte in
/// one allocation, which the row ceiling never sees: it is ONE row. The
/// deadline does not help either — the allocation is fast, it is the memory
/// that hurts. sqlite raises an error instead of allocating past this.
const MAX_VALUE_BYTES: i32 = 8 * 1024 * 1024;

/// Ceiling on the whole response, so many medium rows cannot do what one huge
/// value cannot.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// The sqlite table backing a node, or an error naming the fix.
///
/// A node name may contain `-`, which is a perfectly good address and a
/// subtraction operator in SQL. Rather than silently mangling it, a table node
/// is required to have a name that is already an identifier.
pub fn table_name(name: &NodeName) -> Result<String> {
    name.sqlite_table().ok_or_else(|| {
        anyhow::anyhow!(
            "a table node's name becomes the sqlite table `t_{name}`, so it cannot contain '-' \
             (use '_' instead)"
        )
    })
}

fn column_names(cfg: &TableConfig) -> Vec<String> {
    cfg.columns.iter().map(|c| c.name.to_string()).collect()
}

/// Build `t_<name>` from the node's config, replacing anything at that name.
///
/// **Destructive, and named for the node lifecycle rather than for SQL.** Call
/// this only where a table node has just been CREATED, so the table it backs
/// is new by definition and starts empty. To make sure an EXISTING node's
/// table is there -- engine boot, reconcile, restore -- call [`ensure`], which
/// never drops anything.
///
/// It used to be `CREATE TABLE IF NOT EXISTS`, which read as harmless
/// idempotence and was an adoption hazard: a `t_<name>` that survived an
/// earlier node of the same name was silently inherited by the new one, old
/// rows AND old columns, so the operator saw data they never wrote and a write
/// of their configured columns failed "no such column".
pub fn create(conn: &Connection, name: &NodeName, cfg: &TableConfig) -> Result<()> {
    let table = table_name(name)?;
    drop(conn, name)?;
    let mut ddl = format!("CREATE TABLE {table} (\n  {KEY_COLUMN} TEXT PRIMARY KEY");
    for c in &cfg.columns {
        // `c.name` is an `Ident`: [a-z0-9_] only, so it cannot close the
        // identifier or introduce a second statement.
        ddl.push_str(&format!(",\n  {} {}", c.name, c.column_type.sqlite_type()));
    }
    ddl.push_str("\n)");
    conn.execute_batch(&ddl)
        .with_context(|| format!("creating {table}"))?;
    Ok(())
}

/// Make sure an existing table node's table is there, without touching a row.
///
/// The counterpart to [`create`]: `create` is for a node that has just been
/// made, `ensure` is for one that already exists. Nothing re-ensured the
/// tables on boot, so a project whose nodes survived while its db file was
/// recreated or migrated kept its nodes and lost its tables -- the board shows
/// a table node and every read of it says "no such table".
///
/// Missing columns are added; existing ones are left exactly as they are. A
/// column dropped from the config stays in sqlite rather than being deleted,
/// because this runs unattended at boot and a config edit is not consent to
/// destroy a column's data.
pub fn ensure(conn: &Connection, name: &NodeName, cfg: &TableConfig) -> Result<()> {
    let table = table_name(name)?;
    let mut ddl = format!("CREATE TABLE IF NOT EXISTS {table} (\n  {KEY_COLUMN} TEXT PRIMARY KEY");
    for c in &cfg.columns {
        ddl.push_str(&format!(",\n  {} {}", c.name, c.column_type.sqlite_type()));
    }
    ddl.push_str("\n)");
    conn.execute_batch(&ddl)
        .with_context(|| format!("ensuring {table}"))?;

    let present: std::collections::HashSet<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;
    for c in &cfg.columns {
        if !present.contains(c.name.as_str()) {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {} {}",
                c.name,
                c.column_type.sqlite_type()
            ))
            .with_context(|| format!("adding {table}.{}", c.name))?;
        }
    }
    Ok(())
}

pub fn drop(conn: &Connection, name: &NodeName) -> Result<()> {
    let table = table_name(name)?;
    conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}"))
        .with_context(|| format!("dropping {table}"))?;
    Ok(())
}

/// Follow a node rename, so `t_<name>` keeps matching the address (§4).
pub fn rename(conn: &Connection, from: &NodeName, to: &NodeName) -> Result<()> {
    let (old, new) = (table_name(from)?, table_name(to)?);
    if old == new {
        return Ok(());
    }
    conn.execute_batch(&format!("ALTER TABLE {old} RENAME TO {new}"))
        .with_context(|| format!("renaming {old} to {new}"))?;
    Ok(())
}

/// Upsert one row. The JSON object IS the row: a column the caller omits is
/// set to NULL rather than left at its previous value, so writing a row twice
/// with different fields cannot leave a hybrid of the two behind.
pub fn put_row(
    conn: &Connection,
    name: &NodeName,
    cfg: &TableConfig,
    key: &str,
    value: &Value,
) -> Result<()> {
    let table = table_name(name)?;
    let Value::Object(obj) = value else {
        bail!("a table row must be a JSON object of column values");
    };
    reject_unknown_columns(cfg, obj)?;

    let cols = column_names(cfg);
    let mut binds: Vec<SqlValue> = vec![SqlValue::Text(key.to_string())];
    for c in &cfg.columns {
        let raw = obj.get(c.name.as_str()).unwrap_or(&Value::Null);
        binds.push(to_sql(c.column_type, c.name.as_str(), raw)?);
    }

    let placeholders = std::iter::repeat_n("?", cols.len() + 1)
        .collect::<Vec<_>>()
        .join(",");
    let assignments = cols
        .iter()
        .map(|c| format!("{c}=excluded.{c}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = if cols.is_empty() {
        format!(
            "INSERT INTO {table} ({KEY_COLUMN}) VALUES (?) ON CONFLICT({KEY_COLUMN}) DO NOTHING"
        )
    } else {
        format!(
            "INSERT INTO {table} ({KEY_COLUMN},{}) VALUES ({placeholders}) \
             ON CONFLICT({KEY_COLUMN}) DO UPDATE SET {assignments}",
            cols.join(",")
        )
    };
    conn.execute(&sql, params_from_iter(binds))
        .with_context(|| format!("writing {table}/{key}"))?;
    Ok(())
}

/// A column an agent invented is an error, never a silent no-op: a typo would
/// otherwise report success and write nothing.
fn reject_unknown_columns(cfg: &TableConfig, obj: &Map<String, Value>) -> Result<()> {
    for k in obj.keys() {
        if k == KEY_COLUMN {
            // The key comes from the address, not the body; accepting it here
            // as well would let the two disagree.
            bail!("`{KEY_COLUMN}` comes from the row address, not the body");
        }
        if !cfg.columns.iter().any(|c| c.name.as_str() == k) {
            let known: Vec<&str> = cfg.columns.iter().map(|c| c.name.as_str()).collect();
            bail!(
                "no column {k:?} on this table (columns: {})",
                known.join(", ")
            );
        }
    }
    Ok(())
}

pub fn get_row(
    conn: &Connection,
    name: &NodeName,
    cfg: &TableConfig,
    key: &str,
) -> Result<Option<Value>> {
    let table = table_name(name)?;
    let cols = column_names(cfg);
    let select = if cols.is_empty() {
        KEY_COLUMN.to_string()
    } else {
        format!("{KEY_COLUMN},{}", cols.join(","))
    };
    let sql = format!("SELECT {select} FROM {table} WHERE {KEY_COLUMN} = ?1");
    let row = conn
        .prepare(&sql)?
        .query_row([key], |r| Ok(row_to_json(r, cfg)))
        .optional()
        .with_context(|| format!("reading {table}/{key}"))?;
    row.transpose()
}

pub fn delete_row(conn: &Connection, name: &NodeName, key: &str) -> Result<bool> {
    let table = table_name(name)?;
    let n = conn.execute(
        &format!("DELETE FROM {table} WHERE {KEY_COLUMN} = ?1"),
        [key],
    )?;
    Ok(n > 0)
}

/// Row keys, optionally by prefix. Ordered so paging is stable.
pub fn list_keys(
    conn: &Connection,
    name: &NodeName,
    prefix: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<String>> {
    let table = table_name(name)?;
    let limit = limit.min(MAX_ROWS);
    let sql = format!(
        "SELECT {KEY_COLUMN} FROM {table} WHERE {KEY_COLUMN} LIKE ?1 ESCAPE '\\' \
         ORDER BY {KEY_COLUMN} LIMIT ?2 OFFSET ?3"
    );
    let pattern = format!("{}%", escape_like(prefix.unwrap_or_default()));
    let keys = conn
        .prepare(&sql)?
        .query_map(
            rusqlite::params![pattern, limit as i64, offset as i64],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(keys)
}

/// A prefix is a literal, not a pattern: an agent passing `%` must not get
/// every row back.
fn escape_like(prefix: &str) -> String {
    prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn list_rows(
    conn: &Connection,
    name: &NodeName,
    cfg: &TableConfig,
    limit: usize,
    offset: usize,
) -> Result<Vec<Value>> {
    let table = table_name(name)?;
    let limit = limit.min(MAX_ROWS);
    let cols = column_names(cfg);
    let select = if cols.is_empty() {
        KEY_COLUMN.to_string()
    } else {
        format!("{KEY_COLUMN},{}", cols.join(","))
    };
    let sql = format!("SELECT {select} FROM {table} ORDER BY {KEY_COLUMN} LIMIT ?1 OFFSET ?2");
    let rows = conn
        .prepare(&sql)?
        .query_map(rusqlite::params![limit as i64, offset as i64], |r| {
            Ok(row_to_json(r, cfg))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

pub fn count_rows(conn: &Connection, name: &NodeName) -> Result<u64> {
    let table = table_name(name)?;
    let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
    Ok(n as u64)
}

fn row_to_json(row: &rusqlite::Row<'_>, cfg: &TableConfig) -> Result<Value> {
    let mut obj = Map::new();
    obj.insert(
        KEY_COLUMN.into(),
        Value::String(row.get::<_, String>(0).unwrap_or_default()),
    );
    for (i, c) in cfg.columns.iter().enumerate() {
        let v = from_sql(c.column_type, row.get_ref(i + 1)?)?;
        obj.insert(c.name.to_string(), v);
    }
    Ok(Value::Object(obj))
}

/// Convert one JSON field to its sqlite representation, refusing a mismatch
/// rather than coercing it. An agent that writes a string into an integer
/// column has a bug, and silently storing `0` would hide it.
fn to_sql(ty: ColumnType, name: &str, v: &Value) -> Result<SqlValue> {
    let wrong = |want: &str| anyhow::anyhow!("column {name:?} is {want}, got {}", kind_of(v));
    Ok(match (ty, v) {
        (_, Value::Null) => SqlValue::Null,
        (ColumnType::Text, Value::String(s)) => SqlValue::Text(s.clone()),
        (ColumnType::Text, _) => return Err(wrong("text")),
        (ColumnType::Integer, Value::Number(n)) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => return Err(wrong("an integer")),
        },
        (ColumnType::Integer, Value::Bool(b)) => SqlValue::Integer(*b as i64),
        (ColumnType::Integer, _) => return Err(wrong("an integer")),
        (ColumnType::Real, Value::Number(n)) => match n.as_f64() {
            Some(f) => SqlValue::Real(f),
            None => return Err(wrong("a real")),
        },
        (ColumnType::Real, _) => return Err(wrong("a real")),
        (ColumnType::Blob, Value::String(s)) => SqlValue::Blob(
            B64.decode(s)
                .with_context(|| format!("column {name:?} is a blob, so it must be base64"))?,
        ),
        (ColumnType::Blob, _) => return Err(wrong("a base64 blob")),
        // Stored as TEXT; anything that is valid JSON is valid here, which is
        // the point of the column type.
        (ColumnType::Json, other) => SqlValue::Text(serde_json::to_string(other)?),
    })
}

fn from_sql(ty: ColumnType, v: ValueRef<'_>) -> Result<Value> {
    Ok(match (ty, v) {
        (_, ValueRef::Null) => Value::Null,
        (ColumnType::Json, ValueRef::Text(t)) => {
            let s = std::str::from_utf8(t)?;
            // Round-trip as JSON so the caller gets the value they wrote, not
            // a string containing it.
            serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
        }
        (ColumnType::Blob, ValueRef::Blob(b)) => Value::String(B64.encode(b)),
        (_, ValueRef::Text(t)) => Value::String(String::from_utf8_lossy(t).into_owned()),
        (_, ValueRef::Integer(i)) => Value::Number(i.into()),
        (_, ValueRef::Real(f)) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        (_, ValueRef::Blob(b)) => Value::String(B64.encode(b)),
    })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

// --- user SQL --------------------------------------------------------------

/// Run a read-only `SELECT` scoped to exactly one table.
///
/// This is the only place a string an agent composed reaches sqlite, so it is
/// deliberately paranoid and layered — each of these alone would be an
/// argument, together they are a boundary:
///
/// 1. A SEPARATE connection, opened `READ_ONLY`, so nothing here can touch the
///    engine's own connection or its transactions.
/// 2. An authorizer that DENIES by default and allows reading exactly one
///    table. `sqlite_master` is denied too: the set of tables on the board is
///    itself information the querying node may not be wired to.
/// 3. One statement only, so `; DROP ...` is refused before sqlite sees it.
/// 4. A deadline, because `WITH RECURSIVE` can loop without touching a table
///    at all and the authorizer would never be consulted again.
pub fn query(db_path: &Path, table: &str, sql: &str) -> Result<Value> {
    let sql = sql.trim();
    if sql.is_empty() {
        bail!("empty query");
    }

    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", db_path.display()))?;

    // Bound a single value before anything runs. `set_limit` is checked by
    // sqlite as it builds each result, so an oversized blob is an error rather
    // than an allocation.
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_VALUE_BYTES,
    );

    let allowed = table.to_string();
    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
        authorize(&allowed, ctx.action)
    }));

    let deadline = Instant::now() + QUERY_TIMEOUT;
    conn.progress_handler(PROGRESS_STEPS, Some(move || Instant::now() > deadline));

    // `prepare` stops at the first statement and ignores the rest, so
    // `SELECT 1; DROP TABLE x` would run the SELECT and silently discard the
    // rest. Refusing is honest; the caller should know their query was not
    // what ran.
    if has_extra_statement(sql) {
        bail!("only one statement per query");
    }
    let mut stmt = conn.prepare(sql).map_err(explain)?;
    if !stmt.readonly() {
        bail!("only read-only queries are allowed here");
    }

    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([]).map_err(explain)?;
    // Rows are consumed one at a time and counted as they arrive: the ceiling
    // is a fetch limit, not a truncation of something already in memory, so a
    // cartesian self-join costs 10,000 rows rather than all of them.
    let mut out = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(explain)? {
        if out.len() >= MAX_ROWS {
            break;
        }
        let mut obj = Map::new();
        for (i, name) in names.iter().enumerate() {
            let v = untyped(row.get_ref(i)?);
            bytes += value_bytes(&v) + name.len();
            obj.insert(name.clone(), v);
        }
        out.push(Value::Object(obj));
        if bytes > MAX_RESPONSE_BYTES {
            bail!(
                "the result exceeded {} MiB; narrow the query or page it",
                MAX_RESPONSE_BYTES / (1024 * 1024)
            );
        }
    }
    Ok(Value::Array(out))
}

/// sqlite reports an authorizer refusal and a deadline abort with wording that
/// tells an agent nothing about what to do differently.
fn explain(e: rusqlite::Error) -> anyhow::Error {
    let msg = e.to_string();
    // sqlite says "access to t_secrets.key is prohibited", which names the
    // exact object and is better than anything generic -- so it is kept, and
    // only the reason is added.
    if msg.contains("not authorized") || msg.contains("prohibited") {
        anyhow::anyhow!(
            "not authorized: {msg} -- a query may only read the table it is addressed to, \
             and may not use ATTACH, PRAGMA or the schema tables"
        )
    } else if msg.contains("interrupted") {
        anyhow::anyhow!("query took longer than {}s", QUERY_TIMEOUT.as_secs())
    } else if msg.contains("too big") {
        anyhow::anyhow!(
            "a single value exceeded {} MiB: {msg}",
            MAX_VALUE_BYTES / (1024 * 1024)
        )
    } else {
        anyhow::anyhow!(msg)
    }
}

/// Default DENY (§3). Anything not named here is refused, so a sqlite version
/// that adds an action does not quietly widen this.
fn authorize(allowed: &str, action: AuthAction<'_>) -> Authorization {
    match action {
        AuthAction::Select => Authorization::Allow,
        // Case-INSENSITIVE, because sqlite identifiers are: `T_NOTES` and
        // `sqlite_MASTER` name the same objects as their lowercase spellings.
        // Safe as an allow rule because node names are lowercase by
        // construction, so no two tables can differ only by case.
        AuthAction::Read { table_name, .. } if table_name.eq_ignore_ascii_case(allowed) => {
            Authorization::Allow
        }
        // Common table expressions and subqueries are fine; they still read
        // through `Read`, which is checked above.
        AuthAction::Recursive => Authorization::Allow,
        // Aggregates, string and date functions. `load_extension` is a
        // function too, and would be a way out of this box entirely.
        AuthAction::Function { function_name } => {
            if function_name.eq_ignore_ascii_case("load_extension") {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }
        _ => Authorization::Deny,
    }
}

/// A cheap check for a second statement. Only used to turn sqlite's silent
/// truncation into an error, never as the security boundary — the authorizer
/// and the read-only connection are that.
fn has_extra_statement(sql: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in sql.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => {
                return !sql[i + 1..].trim().is_empty();
            }
            _ => {}
        }
    }
    false
}

fn value_bytes(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Null => 4,
        _ => 8,
    }
}

/// A query's projection can be an expression, so the declared column types do
/// not apply: report whatever sqlite actually produced.
fn untyped(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(B64.encode(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// `create` builds from the config it is given, over anything already
    /// standing at that name.
    ///
    /// With the orphaning route closed in `board::update` nothing SHOULD ever
    /// be standing there, which is precisely why this is tested here: the
    /// board-level test cannot reach this line any more, and an untested belt
    /// is a belt that quietly rots back into `CREATE TABLE IF NOT EXISTS`.
    #[test]
    fn create_claims_the_name_rather_than_adopting_what_is_there() {
        let conn = crate::db::open_memory().unwrap();
        let name = NodeName::new("ledger").unwrap();
        let old = wheel_core::TableConfig {
            columns: vec![wheel_core::Column {
                name: wheel_core::Ident::new("amount").unwrap(),
                column_type: wheel_core::ColumnType::Text,
            }],
        };
        create(&conn, &name, &old).unwrap();
        put_row(&conn, &name, &old, "r1", &serde_json::json!({"amount":"40000"})).unwrap();

        let new = wheel_core::TableConfig {
            columns: vec![wheel_core::Column {
                name: wheel_core::Ident::new("note").unwrap(),
                column_type: wheel_core::ColumnType::Text,
            }],
        };
        create(&conn, &name, &new).unwrap();

        assert!(
            list_rows(&conn, &name, &new, 10, 0).unwrap().is_empty(),
            "the rows of the table that was there were adopted"
        );
        put_row(&conn, &name, &new, "r1", &serde_json::json!({"note":"fresh"}))
            .expect("the rebuilt table must accept the configured columns");
    }
    use wheel_core::{Column, ColumnType, TableConfig};

    pub(super) fn cfg() -> TableConfig {
        TableConfig {
            columns: vec![
                Column {
                    name: wheel_core::Ident::new("title").unwrap(),
                    column_type: ColumnType::Text,
                },
                Column {
                    name: wheel_core::Ident::new("count").unwrap(),
                    column_type: ColumnType::Integer,
                },
                Column {
                    name: wheel_core::Ident::new("meta").unwrap(),
                    column_type: ColumnType::Json,
                },
            ],
        }
    }

    fn name() -> NodeName {
        "notes".parse().unwrap()
    }

    fn db() -> Connection {
        let conn = crate::db::open_memory().unwrap();
        create(&conn, &name(), &cfg()).unwrap();
        conn
    }

    fn put(conn: &Connection, key: &str, v: Value) {
        put_row(conn, &name(), &cfg(), key, &v).unwrap()
    }

    #[test]
    fn a_row_round_trips_through_every_column_type() {
        let conn = db();
        put(
            &conn,
            "r1",
            serde_json::json!({"title": "hello", "count": 3, "meta": {"a": [1, 2]}}),
        );
        let got = get_row(&conn, &name(), &cfg(), "r1").unwrap().unwrap();
        assert_eq!(got["key"], "r1");
        assert_eq!(got["title"], "hello");
        assert_eq!(got["count"], 3);
        // A json column comes back as the value that was written, not as a
        // string containing it.
        assert_eq!(got["meta"], serde_json::json!({"a": [1, 2]}));
    }

    #[test]
    fn a_missing_row_is_none_rather_than_an_error() {
        let conn = db();
        assert!(get_row(&conn, &name(), &cfg(), "nope").unwrap().is_none());
    }

    /// The JSON object IS the row: writing it again with fewer fields must not
    /// leave a hybrid of the two writes behind.
    #[test]
    fn an_upsert_replaces_the_row_rather_than_merging_it() {
        let conn = db();
        put(
            &conn,
            "r1",
            serde_json::json!({"title": "first", "count": 1}),
        );
        put(&conn, "r1", serde_json::json!({"count": 2}));

        let got = get_row(&conn, &name(), &cfg(), "r1").unwrap().unwrap();
        assert_eq!(got["count"], 2);
        assert_eq!(got["title"], Value::Null, "the old title must not survive");
        assert_eq!(count_rows(&conn, &name()).unwrap(), 1, "still one row");
    }

    /// A typo'd column that reported success and wrote nothing would be
    /// invisible until someone read the row back and found it empty.
    #[test]
    fn a_column_that_does_not_exist_is_an_error_not_a_silent_no_op() {
        let conn = db();
        let err = put_row(
            &conn,
            &name(),
            &cfg(),
            "r1",
            &serde_json::json!({"titel": "typo"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("titel"), "{err}");
        assert!(
            err.contains("title"),
            "the error must list the real columns: {err}"
        );
    }

    #[test]
    fn the_key_comes_from_the_address_not_the_body() {
        let conn = db();
        let err = put_row(
            &conn,
            &name(),
            &cfg(),
            "r1",
            &serde_json::json!({"key": "r2"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("row address"), "{err}");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused_rather_than_coerced() {
        let conn = db();
        for bad in [
            serde_json::json!({"count": "three"}),
            serde_json::json!({"title": 5}),
            serde_json::json!({"count": 1.5}),
        ] {
            assert!(
                put_row(&conn, &name(), &cfg(), "r", &bad).is_err(),
                "{bad} should not be accepted"
            );
        }
        // ...but null is always allowed, and so is an absent column.
        put(&conn, "r", serde_json::json!({"title": null}));
        put(&conn, "r2", serde_json::json!({}));
    }

    #[test]
    fn a_row_must_be_an_object() {
        let conn = db();
        assert!(put_row(&conn, &name(), &cfg(), "r", &serde_json::json!([1])).is_err());
        assert!(put_row(&conn, &name(), &cfg(), "r", &serde_json::json!("x")).is_err());
    }

    #[test]
    fn keys_list_in_order_and_delete_reports_whether_it_removed_anything() {
        let conn = db();
        for k in ["b", "a", "c"] {
            put(&conn, k, serde_json::json!({}));
        }
        assert_eq!(
            list_keys(&conn, &name(), None, 100, 0).unwrap(),
            ["a", "b", "c"]
        );
        assert_eq!(list_keys(&conn, &name(), None, 2, 1).unwrap(), ["b", "c"]);
        assert!(delete_row(&conn, &name(), "b").unwrap());
        assert!(!delete_row(&conn, &name(), "b").unwrap(), "already gone");
        assert_eq!(list_keys(&conn, &name(), None, 100, 0).unwrap(), ["a", "c"]);
    }

    /// A prefix is a literal. An agent passing `%` must not get the whole
    /// table back, and `_` must not match an arbitrary character.
    #[test]
    fn a_prefix_is_matched_literally_not_as_a_like_pattern() {
        let conn = db();
        for k in ["a_1", "ax1", "b%c", "bzc"] {
            put(&conn, k, serde_json::json!({}));
        }
        assert_eq!(
            list_keys(&conn, &name(), Some("a_"), 100, 0).unwrap(),
            ["a_1"]
        );
        assert_eq!(
            list_keys(&conn, &name(), Some("b%"), 100, 0).unwrap(),
            ["b%c"]
        );
        assert!(list_keys(&conn, &name(), Some("%"), 100, 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_read_never_returns_more_than_the_ceiling() {
        let conn = db();
        put(&conn, "only", serde_json::json!({}));
        // The ceiling is applied to the caller's limit, however large.
        assert_eq!(
            list_keys(&conn, &name(), None, usize::MAX, 0)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_rows(&conn, &name(), &cfg(), usize::MAX, 0)
                .unwrap()
                .len(),
            1
        );
    }

    /// A node name is an address and may contain `-`; a sqlite identifier may
    /// not. Refusing beats silently mangling it into a table nobody addressed.
    #[test]
    fn a_table_node_name_that_is_not_an_identifier_is_refused_with_the_fix() {
        let bad: NodeName = "my-notes".parse().unwrap();
        let err = table_name(&bad).unwrap_err().to_string();
        assert!(err.contains("my-notes"), "{err}");
        assert!(err.contains("'_'"), "the error must say what to do: {err}");
        assert_eq!(table_name(&name()).unwrap(), "t_notes");
    }

    #[test]
    fn renaming_the_node_renames_the_table_and_keeps_the_rows() {
        let conn = db();
        put(&conn, "r1", serde_json::json!({"title": "kept"}));
        let to: NodeName = "archive".parse().unwrap();
        rename(&conn, &name(), &to).unwrap();

        assert_eq!(count_rows(&conn, &to).unwrap(), 1);
        assert!(count_rows(&conn, &name()).is_err(), "the old table is gone");
        let got = get_row(&conn, &to, &cfg(), "r1").unwrap().unwrap();
        assert_eq!(got["title"], "kept");
    }

    #[test]
    fn create_is_idempotent_and_drop_removes_the_table() {
        let conn = db();
        create(&conn, &name(), &cfg()).unwrap();
        put(&conn, "r", serde_json::json!({}));
        drop(&conn, &name()).unwrap();
        assert!(count_rows(&conn, &name()).is_err());
        drop(&conn, &name()).unwrap();
    }
}

/// `wheel query` is the one place an agent's own string reaches sqlite, so
/// these are written from the attacker's side: each names what it would win.
#[cfg(test)]
mod query_tests {
    use super::*;

    /// A file-backed db, because `query` deliberately opens its OWN read-only
    /// connection and an in-memory database is private to one connection.
    fn file_db() -> (std::path::PathBuf, Connection, NodeName, NodeName) {
        let dir = std::env::temp_dir().join(format!(
            "wheel-tq-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let conn = crate::db::open(&path).unwrap();

        let mine: NodeName = "notes".parse().unwrap();
        let other: NodeName = "secrets".parse().unwrap();
        let c = super::tests::cfg();
        create(&conn, &mine, &c).unwrap();
        create(&conn, &other, &c).unwrap();
        put_row(
            &conn,
            &mine,
            &c,
            "r1",
            &serde_json::json!({"title": "mine", "count": 1}),
        )
        .unwrap();
        put_row(
            &conn,
            &other,
            &c,
            "r1",
            &serde_json::json!({"title": "NOT-FOR-YOU", "count": 9}),
        )
        .unwrap();
        (path, conn, mine, other)
    }

    fn run(path: &Path, sql: &str) -> Result<Value> {
        query(path, "t_notes", sql)
    }

    fn denied(path: &Path, sql: &str) {
        let err = run(path, sql)
            .map(|v| format!("returned {v}"))
            .unwrap_or_else(|e| e.to_string());
        assert!(
            err.contains("not authorized")
                || err.contains("only one statement")
                || err.contains("read-only")
                || err.contains("no such")
                || err.contains("exceeded")
                || err.contains("longer than"),
            "{sql:?} should have been refused, got: {err}"
        );
        assert!(
            !err.starts_with("returned"),
            "{sql:?} was ALLOWED and returned data: {err}"
        );
    }

    /// The control: if everything below is denied because everything is
    /// denied, the box is useless rather than safe.
    #[test]
    fn the_table_it_is_addressed_to_is_actually_queryable() {
        let (path, _c, _m, _o) = file_db();
        let rows = run(&path, "SELECT key, title FROM t_notes ORDER BY key").unwrap();
        assert_eq!(rows[0]["title"], "mine");

        // Aggregates and expressions, which is most of why an agent would use
        // SQL rather than `wheel ls`.
        let agg = run(
            &path,
            "SELECT COUNT(*) AS n, SUM(count) AS total FROM t_notes",
        )
        .unwrap();
        assert_eq!(agg[0]["n"], 1);
        assert_eq!(agg[0]["total"], 1);

        let expr = run(&path, "SELECT UPPER(title) AS t FROM t_notes").unwrap();
        assert_eq!(expr[0]["t"], "MINE");
    }

    /// The whole point: another node's table is another node's data, and a
    /// wire to `notes` is not a wire to `secrets`.
    #[test]
    fn another_nodes_table_is_invisible_however_it_is_reached() {
        let (path, _c, _m, _o) = file_db();
        for sql in [
            "SELECT * FROM t_secrets",
            "SELECT title FROM t_notes UNION SELECT title FROM t_secrets",
            "SELECT (SELECT title FROM t_secrets LIMIT 1) AS leaked",
            "SELECT * FROM t_notes JOIN t_secrets",
            "WITH x AS (SELECT * FROM t_secrets) SELECT * FROM x",
            "SELECT * FROM t_notes WHERE key IN (SELECT key FROM t_secrets)",
        ] {
            denied(&path, sql);
        }
    }

    /// sqlite identifiers are case-insensitive, so a case-sensitive allow rule
    /// would be bypassed by shouting.
    #[test]
    fn case_does_not_get_a_query_out_of_its_table() {
        let (path, _c, _m, _o) = file_db();
        for sql in [
            "SELECT * FROM T_SECRETS",
            "SELECT * FROM t_SeCrEtS",
            "SELECT name FROM SQLITE_MASTER",
            "SELECT name FROM sqlite_MASTER",
        ] {
            denied(&path, sql);
        }
        // ...and the allowed table still works when shouted, because the rule
        // is case-insensitive on the allow side rather than a case-sensitive
        // deny-list.
        assert!(run(&path, "SELECT COUNT(*) FROM T_NOTES").is_ok());
    }

    /// The board's shape is itself information: which tables exist tells an
    /// agent what it is not wired to.
    #[test]
    fn the_schema_is_not_enumerable() {
        let (path, _c, _m, _o) = file_db();
        for sql in [
            "SELECT name FROM sqlite_master",
            "SELECT sql FROM sqlite_master WHERE type='table'",
            "SELECT * FROM sqlite_schema",
            "SELECT * FROM pragma_table_list",
            "SELECT * FROM nodes",
            "SELECT * FROM vault_values",
            "SELECT * FROM messages",
            "SELECT * FROM node_tokens",
        ] {
            denied(&path, sql);
        }
    }

    #[test]
    fn nothing_can_write_however_it_is_phrased() {
        let (path, _c, _m, _o) = file_db();
        for sql in [
            "DELETE FROM t_notes",
            "UPDATE t_notes SET title='x'",
            "INSERT INTO t_notes (key) VALUES ('x')",
            "DROP TABLE t_notes",
            "ALTER TABLE t_notes RENAME TO t_other",
            "CREATE TABLE evil (a TEXT)",
            "CREATE TRIGGER t AFTER INSERT ON t_notes BEGIN SELECT 1; END",
            "CREATE VIEW v AS SELECT * FROM t_secrets",
        ] {
            denied(&path, sql);
        }
        // The data is genuinely untouched.
        let rows = run(&path, "SELECT COUNT(*) AS n FROM t_notes").unwrap();
        assert_eq!(rows[0]["n"], 1);
    }

    #[test]
    fn attach_detach_and_pragma_are_refused() {
        let (path, _c, _m, _o) = file_db();
        for sql in [
            "ATTACH DATABASE '/tmp/evil.db' AS evil",
            "ATTACH ':memory:' AS m",
            "DETACH DATABASE main",
            "PRAGMA table_info(t_secrets)",
            "PRAGMA database_list",
            "PRAGMA journal_mode",
        ] {
            denied(&path, sql);
        }
    }

    /// `prepare` stops at the first statement and silently discards the rest,
    /// so a caller could believe a second statement ran when it did not.
    #[test]
    fn a_second_statement_is_refused_rather_than_silently_dropped() {
        let (path, _c, _m, _o) = file_db();
        denied(&path, "SELECT 1; DROP TABLE t_notes");
        denied(&path, "SELECT * FROM t_notes; SELECT * FROM t_secrets");
        // A semicolon inside a string is not a second statement, and a
        // trailing one is not either.
        assert!(run(&path, "SELECT ';' AS s").is_ok());
        assert!(run(&path, "SELECT 1 AS n;").is_ok());
        assert!(run(&path, "SELECT 1 AS n;   ").is_ok());
    }

    /// One row can be enormous, and the row ceiling never sees it.
    #[test]
    fn one_huge_value_cannot_exhaust_memory() {
        let (path, _c, _m, _o) = file_db();
        denied(&path, "SELECT randomblob(1000000000) AS b");
        denied(&path, "SELECT zeroblob(500000000) AS b");
        denied(&path, "SELECT hex(zeroblob(100000000)) AS b");
        // A reasonable blob is still fine.
        assert!(run(&path, "SELECT length(randomblob(1024)) AS n").is_ok());
    }

    /// Loading an extension would be a way out of the authorizer entirely.
    #[test]
    fn extensions_cannot_be_loaded() {
        let (path, _c, _m, _o) = file_db();
        denied(&path, "SELECT load_extension('/tmp/evil.so')");
        denied(&path, "SELECT LOAD_EXTENSION('/tmp/evil.so')");
    }

    /// A recursive CTE can spin without ever touching a table, so the
    /// authorizer would never be consulted again. The deadline is what stops
    /// it.
    #[test]
    fn an_unbounded_recursive_query_is_stopped_by_the_deadline() {
        let (path, _c, _m, _o) = file_db();
        let started = Instant::now();
        let err = run(
            &path,
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) \
             SELECT SUM(x) AS s FROM c",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("longer than"), "{err}");
        assert!(
            started.elapsed() < QUERY_TIMEOUT * 3,
            "the deadline must actually bound it, took {:?}",
            started.elapsed()
        );
    }

    /// A self-join multiplies rows without any single value being large.
    #[test]
    fn a_cartesian_join_is_bounded_by_the_row_ceiling() {
        let (path, conn, mine, _o) = file_db();
        let c = super::tests::cfg();
        for i in 0..60 {
            put_row(&conn, &mine, &c, &format!("k{i}"), &serde_json::json!({})).unwrap();
        }
        // 60^3 = 216,000 rows available; we must stop at the ceiling.
        let rows = run(
            &path,
            "SELECT a.key AS x FROM t_notes a, t_notes b, t_notes c",
        )
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), MAX_ROWS);
    }

    #[test]
    fn an_empty_query_says_so() {
        let (path, _c, _m, _o) = file_db();
        assert!(run(&path, "   ").unwrap_err().to_string().contains("empty"));
    }
}
