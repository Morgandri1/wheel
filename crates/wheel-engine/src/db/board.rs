//! Board storage: nodes and wires.
//!
//! Every mutation validates through `wheel-core` — the same functions the API
//! calls — so the two cannot disagree about what a legal board is.

use anyhow::Result;

use super::tables;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;
use wheel_core::{
    check_wire, AgentState, Node, NodeConfig, NodeName, NodeType, Position, Timestamp, Wire,
    WireType,
};

#[derive(Debug, thiserror::Error)]
pub enum BoardError {
    /// Two vaults an agent can read supply the same key. Refused rather than
    /// resolved: choosing one silently bills a real person's account.
    #[error("{0}")]
    Ambiguous(String),
    #[error("no such node: {0}")]
    NotFound(String),
    #[error("a node named {0:?} already exists")]
    NameTaken(String),
    #[error(transparent)]
    Wire(#[from] wheel_core::WireError),
    #[error(transparent)]
    Config(#[from] wheel_core::ConfigError),
    /// A table node's storage could not follow it. Its own message says why.
    #[error("{0}")]
    Storage(String),
}

fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let id: String = row.get("id")?;
    let name: String = row.get("name")?;
    let ty: String = row.get("type")?;
    let config: String = row.get("config")?;

    // `type` and `config` are stored in separate columns but are one value in
    // the domain, so they are rejoined through the canonical adjacently-tagged
    // representation rather than matched on by hand here.
    let tagged = serde_json::json!({ "type": ty, "config": serde_json::from_str::<serde_json::Value>(&config).unwrap_or(serde_json::Value::Null) });
    let config: NodeConfig = serde_json::from_value(tagged).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Node {
        id: id.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        name: NodeName::new(name).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        position: Position::new(row.get("x")?, row.get("y")?),
        wires: Vec::new(),
        config,
    })
}

/// Split a `NodeConfig` back into the `(type, config-json)` pair the schema
/// stores.
fn split_config(config: &NodeConfig) -> (String, String) {
    let v = serde_json::to_value(config).expect("a node config always serializes");
    let ty = v["type"].as_str().unwrap_or_default().to_string();
    let cfg = v
        .get("config")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    (ty, cfg.to_string())
}

pub fn create(conn: &Connection, node: &Node) -> Result<(), BoardError> {
    create_with(conn, node, &[])
}

/// As [`create`], honouring the engine's SSRF allowlist for `tool` nodes.
///
/// Empty in production, where the engine refuses to boot with one set, so this
/// is the same function with the same answers there (ADVERSARY 027).
pub fn create_with(
    conn: &Connection,
    node: &Node,
    allow_hosts: &[String],
) -> Result<(), BoardError> {
    wheel_core::validate_config_with(&node.config, allow_hosts)?;
    let (ty, cfg) = split_config(&node.config);
    let now = Timestamp::now().to_rfc3339();

    conn.execute(
        "INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
        params![
            node.id.to_string(),
            node.name.as_str(),
            ty,
            cfg,
            node.position.x,
            node.position.y,
            now
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            BoardError::NameTaken(node.name.to_string())
        }
        other => BoardError::NotFound(other.to_string()),
    })?;

    if node.node_type() == NodeType::Agent {
        conn.execute(
            "INSERT INTO agent_state (node_id,status) VALUES (?1,'stopped')",
            params![node.id.to_string()],
        )
        .ok();
    }
    // A table node IS its table. Doing this here rather than in the route
    // means no caller can create one without its storage -- and a name that
    // cannot become a sqlite identifier fails the whole creation instead of
    // leaving a node on the board that silently has nowhere to put rows.
    if let NodeConfig::Table(cfg) = &node.config {
        // The name rule for table nodes is stricter than for other node types
        // (PM ruling): it becomes a sqlite identifier, so it must already be
        // one. Checked here, before the DDL, so the message names the rule
        // rather than reporting a SQL syntax error.
        if let Err(e) = wheel_core::validate_table_name(node.name.as_str()) {
            conn.execute(
                "DELETE FROM nodes WHERE id = ?1",
                params![node.id.to_string()],
            )
            .ok();
            return Err(BoardError::Storage(e.to_string()));
        }
        if let Err(e) = tables::create(conn, &node.name, cfg) {
            conn.execute(
                "DELETE FROM nodes WHERE id = ?1",
                params![node.id.to_string()],
            )
            .ok();
            return Err(BoardError::Storage(e.to_string()));
        }
    }
    Ok(())
}

pub fn get(conn: &Connection, id: Uuid) -> Result<Option<Node>> {
    let mut node = conn
        .prepare("SELECT * FROM nodes WHERE id = ?1")?
        .query_row(params![id.to_string()], row_to_node)
        .optional()?;
    if let Some(n) = node.as_mut() {
        n.wires = wires_from(conn, id)?;
    }
    Ok(node)
}

pub fn get_by_name(conn: &Connection, name: &str) -> Result<Option<Node>> {
    let mut node = conn
        .prepare("SELECT * FROM nodes WHERE name = ?1")?
        .query_row(params![name], row_to_node)
        .optional()?;
    if let Some(n) = node.as_mut() {
        n.wires = wires_from(conn, n.id)?;
    }
    Ok(node)
}

/// Every node on the board, each with its outgoing wires attached.
pub fn list(conn: &Connection) -> Result<Vec<Node>> {
    let mut stmt = conn.prepare("SELECT * FROM nodes ORDER BY name")?;
    let mut nodes: Vec<Node> = stmt.query_map([], row_to_node)?.collect::<Result<_, _>>()?;
    for n in &mut nodes {
        n.wires = wires_from(conn, n.id)?;
    }
    Ok(nodes)
}

pub fn wires_from(conn: &Connection, from: Uuid) -> Result<Vec<Wire>> {
    let mut stmt =
        conn.prepare("SELECT to_id, type FROM wires WHERE from_id = ?1 ORDER BY to_id, type")?;
    let rows = stmt.query_map(params![from.to_string()], |r| {
        let to: String = r.get(0)?;
        let ty: String = r.get(1)?;
        Ok((to, ty))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (to, ty) = row?;
        out.push(Wire::new(
            to.parse()?,
            serde_json::from_value(serde_json::Value::String(ty))?,
        ));
    }
    Ok(out)
}

/// Wires pointing AT a node — needed for the preamble's "both directions" view
/// and for ctx injection lookup.
pub fn wires_to(conn: &Connection, to: Uuid) -> Result<Vec<(Uuid, WireType)>> {
    let mut stmt =
        conn.prepare("SELECT from_id, type FROM wires WHERE to_id = ?1 ORDER BY from_id, type")?;
    let rows = stmt.query_map(params![to.to_string()], |r| {
        let from: String = r.get(0)?;
        let ty: String = r.get(1)?;
        Ok((from, ty))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (from, ty) = row?;
        out.push((
            from.parse()?,
            serde_json::from_value(serde_json::Value::String(ty))?,
        ));
    }
    Ok(out)
}

pub fn delete(conn: &Connection, id: Uuid) -> Result<bool> {
    // The user's rows are not covered by the schema's cascades: `t_<name>` is
    // a table of its own, so deleting the node has to take it too or the data
    // outlives the thing that addressed it.
    if let Some(node) = get(conn, id)? {
        if let NodeConfig::Table(_) = &node.config {
            // Not `.ok()`. A drop that fails and is ignored deletes the node
            // row anyway, and the tenant's rows outlive the only thing that
            // addressed them -- invisible on the board, and waiting under a
            // name someone can create again. Failing the delete leaves a node
            // the operator can retry; succeeding quietly does not.
            tables::drop(conn, &node.name)?;
        }
    }
    // Wires, agent_state, messages, tokens and logs all cascade via the schema.
    let n = conn.execute("DELETE FROM nodes WHERE id = ?1", params![id.to_string()])?;
    Ok(n > 0)
}

/// Create a wire after checking it against the §3 matrix.
pub fn add_wire(
    conn: &Connection,
    from: Uuid,
    to: Uuid,
    ty: WireType,
    granted_by: Option<Uuid>,
) -> Result<(), BoardError> {
    let from_node = get(conn, from)
        .ok()
        .flatten()
        .ok_or_else(|| BoardError::NotFound(from.to_string()))?;
    let to_node = get(conn, to)
        .ok()
        .flatten()
        .ok_or_else(|| BoardError::NotFound(to.to_string()))?;

    check_wire(from, from_node.node_type(), to, to_node.node_type(), ty)?;

    // Refuse a second vault that would supply a key the agent already gets
    // from another one. Caught here, at the moment the operator makes the
    // wire, because the alternative is an agent that looks correctly
    // configured and silently runs as whichever account won.
    if from_node.node_type() == NodeType::Agent
        && to_node.node_type() == NodeType::Vault
        && ty == WireType::Read
    {
        if let Ok(Some(a)) = crate::vault::find_ambiguity(conn, from, Some(to)) {
            return Err(BoardError::Ambiguous(a.to_string()));
        }
    }

    // Idempotent: re-creating an existing wire is a no-op, not an error.
    conn.execute(
        "INSERT OR IGNORE INTO wires (from_id,to_id,type,granted_by,created_at)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            from.to_string(),
            to.to_string(),
            ty.as_str(),
            granted_by.map(|g| g.to_string()),
            Timestamp::now().to_rfc3339()
        ],
    )
    .map_err(|e| BoardError::NotFound(e.to_string()))?;
    Ok(())
}

/// Update a node's name, position and config in place. The node's TYPE is not
/// updatable: `patch_node` re-tags any config patch with the existing type, so
/// a node can never change what kind of thing it is.
pub fn update(conn: &Connection, node: &Node) -> Result<(), BoardError> {
    update_with(conn, node, &[])
}

/// As [`update`], honouring the engine's SSRF allowlist for `tool` nodes.
pub fn update_with(
    conn: &Connection,
    node: &Node,
    allow_hosts: &[String],
) -> Result<(), BoardError> {
    wheel_core::validate_config_with(&node.config, allow_hosts)?;

    // `t_<name>` is a table of its own, so it has to follow the node through
    // every shape the node can change into. A rename that did not carry the
    // table would leave every row unreachable from the new address; a node
    // that stops being a table and is not dropped leaves the rows behind with
    // nothing on the board addressing them -- and then the next table node to
    // claim that name inherits a stranger's data.
    let was = get(conn, node.id).ok().flatten();
    let was_table = was
        .as_ref()
        .filter(|n| matches!(n.config, NodeConfig::Table(_)))
        .map(|n| n.name.clone());
    match (&was_table, &node.config) {
        (Some(old_name), NodeConfig::Table(_)) if old_name != &node.name => {
            tables::rename(conn, old_name, &node.name)
                .map_err(|e| BoardError::Storage(e.to_string()))?;
        }
        (Some(old_name), c) if !matches!(c, NodeConfig::Table(_)) => {
            tables::drop(conn, old_name).map_err(|e| BoardError::Storage(e.to_string()))?;
        }
        (None, NodeConfig::Table(cfg)) => {
            wheel_core::validate_table_name(node.name.as_str())
                .map_err(|e| BoardError::Storage(e.to_string()))?;
            tables::create(conn, &node.name, cfg).map_err(|e| BoardError::Storage(e.to_string()))?;
        }
        _ => {}
    }

    let (ty, cfg) = split_config(&node.config);
    conn.execute(
        "UPDATE nodes SET name=?2, type=?3, config=?4, x=?5, y=?6, updated_at=?7 WHERE id=?1",
        params![
            node.id.to_string(),
            node.name.as_str(),
            ty,
            cfg,
            node.position.x,
            node.position.y,
            Timestamp::now().to_rfc3339()
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            BoardError::NameTaken(node.name.to_string())
        }
        other => BoardError::NotFound(other.to_string()),
    })?;
    Ok(())
}

/// Observed state for an agent node. Absent rows read as the default
/// (`stopped`, unhosted) rather than erroring, so a board read never fails
/// because a state row has not been written yet.
pub fn agent_state(conn: &Connection, node_id: Uuid) -> Result<AgentState> {
    // Counted, not assumed. This was hardcoded 0, and the UI shows it: an
    // operator authenticating a blocked agent read "0 queued" and concluded
    // their message had been lost, which -- while a real data-loss bug was
    // being fixed -- told a scarier story than the truth.
    let queued: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE to_id = ?1 AND state = 'queued'",
            params![node_id.to_string()],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let s = conn
        .prepare(
            "SELECT status, session_id, last_activity, last_error, hosted_on, turns, usd
             FROM agent_state WHERE node_id = ?1",
        )?
        .query_row(params![node_id.to_string()], |r| {
            let status: String = r.get(0)?;
            let last_activity: Option<String> = r.get(2)?;
            Ok(AgentState {
                status: serde_json::from_value(serde_json::Value::String(status))
                    .unwrap_or_default(),
                session_id: r.get(1)?,
                last_activity: last_activity
                    .and_then(|t| wheel_core::Timestamp::parse_rfc3339(&t).ok()),
                last_error: r.get(3)?,
                hosted_on: r.get(4)?,
                queued_messages: queued as u32,
                spend: Some(wheel_core::Spend {
                    turns: r.get::<_, i64>(5)? as u64,
                    usd: r.get(6)?,
                }),
            })
        })
        .optional()?;
    Ok(s.unwrap_or_default())
}

/// Set an agent's status directly. Used by auth to move a node out of
/// `needs_auth` once credentials arrive, so the queued message that stalled
/// there is delivered on the next start rather than sitting forever.
pub fn set_status(
    conn: &Connection,
    node: Uuid,
    status: wheel_core::AgentStatus,
    err: Option<&str>,
) {
    let _ = conn.execute(
        "INSERT INTO agent_state (node_id,status,last_activity,last_error)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(node_id) DO UPDATE SET status=?2, last_activity=?3, last_error=?4",
        params![
            node.to_string(),
            status.as_str(),
            Timestamp::now().to_rfc3339(),
            err
        ],
    );
}

pub fn remove_wire(conn: &Connection, from: Uuid, to: Uuid, ty: WireType) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM wires WHERE from_id=?1 AND to_id=?2 AND type=?3",
        params![from.to_string(), to.to_string(), ty.as_str()],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::*;

    fn mem() -> Connection {
        crate::db::open_memory().unwrap()
    }

    fn node(name: &str, config: NodeConfig) -> Node {
        Node::new(
            Uuid::new_v4(),
            NodeName::new(name).unwrap(),
            Position::default(),
            config,
        )
    }

    fn agent(name: &str) -> Node {
        node(name, NodeConfig::Agent(AgentConfig::default()))
    }

    fn ctx(name: &str) -> Node {
        node(
            name,
            NodeConfig::Ctx(CtxConfig {
                markdown: "hello".into(),
            }),
        )
    }

    fn table(name: &str, columns: &[&str]) -> Node {
        node(
            name,
            NodeConfig::Table(TableConfig {
                columns: columns
                    .iter()
                    .map(|c| Column {
                        name: Ident::new(*c).unwrap(),
                        column_type: ColumnType::Text,
                    })
                    .collect(),
            }),
        )
    }

    fn table_cfg(n: &Node) -> &TableConfig {
        match &n.config {
            NodeConfig::Table(c) => c,
            _ => unreachable!(),
        }
    }

    /// A table node's rows must not outlive it, and a new node that reuses the
    /// name must get a table built from ITS config -- not adopt what was
    /// there.
    ///
    /// The route matters: deleting a table node has always dropped its table,
    /// so a delete-then-recreate test passes whether or not the adoption bug
    /// is present (mine did, and I only found that by putting the bug back).
    /// The table outlives its node when the node stops BEING a table --
    /// `PATCH /v1/nodes/:id` with a config of another type -- after which the
    /// delete no longer recognises it as one.
    #[test]
    fn a_reused_table_name_does_not_inherit_the_previous_nodes_data_or_columns() {
        let c = mem();

        let mut first = table("ledger", &["amount"]);
        create(&c, &first).unwrap();
        tables::put_row(
            &c,
            &first.name,
            table_cfg(&first),
            "r1",
            &serde_json::json!({ "amount": "40000" }),
        )
        .unwrap();
        // Positive control: the assertions below say nothing unless there was
        // something to inherit.
        assert_eq!(
            tables::list_rows(&c, &first.name, table_cfg(&first), 10, 0)
                .unwrap()
                .len(),
            1
        );

        first.config = NodeConfig::Ctx(CtxConfig {
            markdown: "not a table any more".into(),
        });
        update(&c, &first).unwrap();
        assert!(delete(&c, first.id).unwrap());

        let second = table("ledger", &["note"]);
        create(&c, &second).unwrap();

        // (1) The new node's table is empty. The operator did not write those
        // rows and must not be shown them.
        assert!(
            tables::list_rows(&c, &second.name, table_cfg(&second), 10, 0)
                .unwrap()
                .is_empty(),
            "the new node inherited the previous node's rows"
        );

        // (2) And it is THIS node's schema. A table rebuilt from a default
        // shape would pass (1) and fail here, which is what makes (1) alone
        // an unreliable gate.
        tables::put_row(
            &c,
            &second.name,
            table_cfg(&second),
            "r1",
            &serde_json::json!({ "note": "fresh" }),
        )
        .expect("the rebuilt table must accept the node's configured columns");
        assert_eq!(
            tables::list_rows(&c, &second.name, table_cfg(&second), 10, 0).unwrap(),
            vec![serde_json::json!({ "key": "r1", "note": "fresh" })]
        );
    }

    /// The same orphaning route, stated as its own property: a node that stops
    /// being a table takes its rows with it.
    #[test]
    fn a_node_that_stops_being_a_table_drops_its_rows() {
        let c = mem();
        let mut n = table("ledger", &["amount"]);
        create(&c, &n).unwrap();
        tables::put_row(
            &c,
            &n.name,
            table_cfg(&n),
            "r1",
            &serde_json::json!({ "amount": "40000" }),
        )
        .unwrap();

        n.config = NodeConfig::Ctx(CtxConfig {
            markdown: "not a table any more".into(),
        });
        update(&c, &n).unwrap();

        let surviving: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t_ledger'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(surviving, 0, "t_ledger outlived the table node");
    }

    #[test]
    fn a_node_round_trips_through_storage_with_its_type_intact() {
        let c = mem();
        for n in [
            agent("planner"),
            ctx("notes"),
            node(
                "store",
                NodeConfig::Table(TableConfig {
                    columns: vec![Column {
                        name: Ident::new("title").unwrap(),
                        column_type: ColumnType::Text,
                    }],
                }),
            ),
            node("files", NodeConfig::Chest(ChestConfig {})),
        ] {
            create(&c, &n).unwrap();
            let back = get(&c, n.id).unwrap().expect("node should exist");
            assert_eq!(back, n, "round-trip failed for {}", n.name);
        }
    }

    #[test]
    fn duplicate_names_are_refused_because_a_name_is_an_address() {
        let c = mem();
        create(&c, &agent("worker")).unwrap();
        let err = create(&c, &ctx("worker")).unwrap_err();
        assert!(matches!(err, BoardError::NameTaken(n) if n == "worker"));
    }

    #[test]
    fn an_invalid_config_never_reaches_storage() {
        let c = mem();
        // Endpoint path traversal is rejected by wheel-core's validator, and
        // storage must not be reachable around it.
        let bad = node(
            "hook",
            NodeConfig::Endpoint(EndpointConfig {
                method: HttpMethod::Post,
                path: "/a/../b".into(),
                response_mode: ResponseMode::Ack,
                auth: EndpointAuth::None,
            }),
        );
        assert!(create(&c, &bad).is_err());
        assert!(list(&c).unwrap().is_empty());
    }

    #[test]
    fn wires_are_checked_against_the_matrix_at_the_storage_boundary() {
        let c = mem();
        let a = agent("a");
        let b = agent("b");
        let n = ctx("notes");
        create(&c, &a).unwrap();
        create(&c, &b).unwrap();
        create(&c, &n).unwrap();

        // Allowed.
        add_wire(&c, a.id, b.id, WireType::Send, None).unwrap();
        add_wire(&c, a.id, n.id, WireType::Read, None).unwrap();
        add_wire(&c, n.id, a.id, WireType::Send, None).unwrap();

        // Denied by the matrix: nothing may write to an agent.
        assert!(add_wire(&c, a.id, b.id, WireType::Write, None).is_err());
        // Denied: ctx has no read wire to an agent.
        assert!(add_wire(&c, n.id, a.id, WireType::Read, None).is_err());
        // Denied: self-wire, even though agent->agent send is a legal pair.
        assert!(add_wire(&c, a.id, a.id, WireType::Send, None).is_err());
        // Denied: unknown node.
        assert!(add_wire(&c, a.id, Uuid::new_v4(), WireType::Send, None).is_err());

        let wires = wires_from(&c, a.id).unwrap();
        assert_eq!(wires.len(), 2, "only the two allowed wires were stored");
    }

    #[test]
    fn creating_the_same_wire_twice_is_idempotent() {
        let c = mem();
        let a = agent("a");
        let b = agent("b");
        create(&c, &a).unwrap();
        create(&c, &b).unwrap();
        add_wire(&c, a.id, b.id, WireType::Send, None).unwrap();
        add_wire(&c, a.id, b.id, WireType::Send, None).unwrap();
        assert_eq!(wires_from(&c, a.id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_node_removes_wires_in_both_directions() {
        let c = mem();
        let a = agent("a");
        let n = ctx("notes");
        create(&c, &a).unwrap();
        create(&c, &n).unwrap();
        add_wire(&c, a.id, n.id, WireType::Read, None).unwrap();
        add_wire(&c, n.id, a.id, WireType::Send, None).unwrap();

        assert!(delete(&c, n.id).unwrap());

        // Both the outgoing wire from the deleted node and the incoming wire
        // pointing at it must be gone, or the board would keep a dangling edge.
        assert!(wires_from(&c, a.id).unwrap().is_empty());
        assert!(wires_to(&c, a.id).unwrap().is_empty());
        assert!(get(&c, n.id).unwrap().is_none());
    }

    #[test]
    fn a_node_carries_its_outgoing_wires_when_listed() {
        let c = mem();
        let a = agent("a");
        let n = ctx("notes");
        create(&c, &a).unwrap();
        create(&c, &n).unwrap();
        add_wire(&c, a.id, n.id, WireType::Read, None).unwrap();
        add_wire(&c, a.id, n.id, WireType::Write, None).unwrap();

        let listed = list(&c).unwrap();
        let a_listed = listed.iter().find(|x| x.id == a.id).unwrap();
        assert_eq!(a_listed.wires.len(), 2);
        // ...and only OUTGOING ones (§3: "OUTGOING wires only").
        let n_listed = listed.iter().find(|x| x.id == n.id).unwrap();
        assert!(n_listed.wires.is_empty());
    }

    #[test]
    fn lookup_by_name_works_because_names_are_how_agents_address_nodes() {
        let c = mem();
        let n = ctx("house-style");
        create(&c, &n).unwrap();
        assert_eq!(get_by_name(&c, "house-style").unwrap().unwrap().id, n.id);
        assert!(get_by_name(&c, "nope").unwrap().is_none());
    }

    /// `queued_messages` was hardcoded 0 while a real data-loss bug was being
    /// fixed, so the field said "your message is gone" at exactly the moment
    /// an operator was checking whether it was. The number the UI shows must
    /// come from the messages table, not from a literal.
    #[test]
    fn queued_messages_counts_what_is_actually_queued() {
        let c = mem();
        let a = agent("counted");
        let b = agent("other");
        create(&c, &a).unwrap();
        create(&c, &b).unwrap();
        assert_eq!(agent_state(&c, a.id).unwrap().queued_messages, 0);

        for body in ["one", "two"] {
            crate::db::messages::enqueue(&c, MessageSender::User, a.id, body.into(), None).unwrap();
        }
        crate::db::messages::enqueue(&c, MessageSender::User, b.id, "theirs".into(), None).unwrap();

        assert_eq!(agent_state(&c, a.id).unwrap().queued_messages, 2);
        // ...and it is per agent, not a board-wide total.
        assert_eq!(agent_state(&c, b.id).unwrap().queued_messages, 1);

        // A delivered message is no longer waiting, and a requeued one is
        // waiting again -- which is the needs_auth path an operator watches.
        let m = crate::db::messages::next_for_delivery(&c, a.id, 0)
            .unwrap()
            .unwrap();
        crate::db::messages::advance(&c, m.id, MessageState::Delivered).unwrap();
        assert_eq!(agent_state(&c, a.id).unwrap().queued_messages, 1);

        crate::db::messages::requeue_all_undelivered(&c, a.id, "harness died").unwrap();
        assert_eq!(
            agent_state(&c, a.id).unwrap().queued_messages,
            2,
            "a requeued message must count as queued again"
        );
    }
}
