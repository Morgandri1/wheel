//! Board storage: nodes and wires.
//!
//! Every mutation validates through `wheel-core` — the same functions the API
//! calls — so the two cannot disagree about what a legal board is.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;
use wheel_core::{
    check_wire, validate_config, Node, NodeConfig, NodeName, NodeType, Position, Timestamp, Wire,
    WireType,
};

#[derive(Debug, thiserror::Error)]
pub enum BoardError {
    #[error("no such node: {0}")]
    NotFound(String),
    #[error("a node named {0:?} already exists")]
    NameTaken(String),
    #[error(transparent)]
    Wire(#[from] wheel_core::WireError),
    #[error(transparent)]
    Config(#[from] wheel_core::ConfigError),
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
    validate_config(&node.config)?;
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
}
