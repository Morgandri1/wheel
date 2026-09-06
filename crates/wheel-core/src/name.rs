//! Node names. A node's name is its *address*: it is what agents type in
//! `wheel msg <name>`, what table nodes derive their sqlite table from, and what
//! appears in injected context headers. It is therefore validated hard.
//!
//! Contract (ARCHITECTURE.md §3): `^[a-z0-9][a-z0-9-_]{0,62}$`, unique per project.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Names the engine refuses to assign to a node because they are already
/// meaningful in the message-delivery contract or the CLI surface.
///
/// `user` is the `from_node` the engine stamps on messages originating in the
/// UI (§3 "Message delivery contract"), so a node called `user` would make
/// `wheel msg user ...` ambiguous. The others are reserved for future
/// engine-internal senders so we never have to break someone's board later.
pub const RESERVED_NAMES: &[&str] = &["user", "wheel", "system", "engine"];

pub const NAME_MAX_LEN: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name must not be empty")]
    Empty,
    #[error("name must be at most {NAME_MAX_LEN} characters, got {0}")]
    TooLong(usize),
    #[error("name must start with a lowercase letter or digit, got {0:?}")]
    BadFirstChar(char),
    #[error("name may only contain lowercase letters, digits, '-' and '_', got {0:?}")]
    BadChar(char),
    #[error("{0:?} is a reserved name")]
    Reserved(String),
    #[error(
        "a table node's name becomes the sqlite table `t_<name>`, so it cannot contain {0:?} \
         (use '_' instead)"
    )]
    TableBadChar(char),
    #[error(
        "a table node's name becomes the sqlite table `t_<name>`, so it must start with a \
         lowercase letter, got {0:?}"
    )]
    TableBadFirstChar(char),
}

/// A validated node name. Construct with [`NodeName::new`]; it is impossible to
/// build an invalid one, including via `serde` (deserialization re-validates).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeName(String);

impl NodeName {
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        validate_name(&raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// The sqlite table backing a `table` node: `t_<name>`.
    ///
    /// Safe to interpolate into SQL *only* because the name charset is
    /// restricted to `[a-z0-9_-]` and `-` is additionally rejected here.
    pub fn sqlite_table(&self) -> Option<String> {
        if self.0.contains('-') {
            None
        } else {
            Some(format!("t_{}", self.0))
        }
    }
}

/// Validate a candidate node name against the §3 contract.
pub fn validate_name(raw: &str) -> Result<(), NameError> {
    let mut chars = raw.chars();
    let first = chars.next().ok_or(NameError::Empty)?;

    // Count in chars, not bytes: a multi-byte char would otherwise report a
    // confusing length. Any non-ASCII char is rejected below anyway.
    let len = raw.chars().count();
    if len > NAME_MAX_LEN {
        return Err(NameError::TooLong(len));
    }

    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(NameError::BadFirstChar(first));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
            return Err(NameError::BadChar(c));
        }
    }

    if RESERVED_NAMES.contains(&raw) {
        return Err(NameError::Reserved(raw.to_string()));
    }
    Ok(())
}

/// A table node's name is stricter than a node address: it becomes the sqlite
/// identifier `t_<name>`, so it must already be one (§3, PM ruling
/// 2026-09-06): `^[a-z][a-z0-9_]{0,62}$`.
///
/// Refused rather than rewritten. Silently turning `table-1` into `table_1`
/// would give the operator a node at an address they did not choose, and every
/// `wheel read table-1` afterwards would fail for a reason nothing explains.
pub fn validate_table_name(raw: &str) -> Result<(), NameError> {
    validate_name(raw)?;
    match raw.chars().next() {
        Some(c) if c.is_ascii_lowercase() => {}
        Some(c) => return Err(NameError::TableBadFirstChar(c)),
        None => return Err(NameError::Empty),
    }
    if let Some(c) = raw.chars().find(|c| *c == '-') {
        return Err(NameError::TableBadChar(c));
    }
    Ok(())
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NodeName {
    type Err = NameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl AsRef<str> for NodeName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for NodeName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NodeName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for NodeName {
    fn schema_name() -> String {
        "NodeName".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, SchemaObject, StringValidation};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                max_length: Some(NAME_MAX_LEN as u32),
                min_length: Some(1),
                pattern: Some("^[a-z0-9][a-z0-9-_]{0,62}$".into()),
            })),
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "A node's unique, addressable name. Reserved: user, wheel, system, engine."
                        .into(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

// ---------------------------------------------------------------------------
// Idents (table column names)
// ---------------------------------------------------------------------------

/// A validated identifier used where we must interpolate into SQL but the
/// *node* reserved-name list does not apply — currently `table` column names.
///
/// Same charset as [`NodeName`] (so quoting into DDL is safe) but `user`,
/// `system` etc. are perfectly reasonable column names, and `-` is rejected
/// outright since these become bare sqlite column identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident(String);

impl Ident {
    pub fn new(raw: impl Into<String>) -> Result<Self, NameError> {
        let raw = raw.into();
        let mut chars = raw.chars();
        let first = chars.next().ok_or(NameError::Empty)?;
        let len = raw.chars().count();
        if len > NAME_MAX_LEN {
            return Err(NameError::TooLong(len));
        }
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(NameError::BadFirstChar(first));
        }
        for c in chars {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' {
                return Err(NameError::BadChar(c));
            }
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Ident {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Ident {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Ident {
    fn schema_name() -> String {
        "Ident".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, Metadata, SchemaObject, StringValidation};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                max_length: Some(NAME_MAX_LEN as u32),
                min_length: Some(1),
                pattern: Some("^[a-z0-9][a-z0-9_]{0,62}$".into()),
            })),
            metadata: Some(Box::new(Metadata {
                description: Some("A sqlite-safe identifier (table column name).".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shapes_the_contract_allows_are_accepted() {
        for ok in [
            "a",
            "0",
            "researcher",
            "agent-1",
            "agent_1",
            "a-b_c-9",
            &"x".repeat(NAME_MAX_LEN),
        ] {
            assert!(NodeName::new(ok).is_ok(), "{ok:?} should be a valid name");
        }
    }

    #[test]
    fn a_name_must_start_with_a_lowercase_letter_or_digit() {
        // Leading '-' and '_' are the interesting ones: both are legal LATER
        // in a name, so only the first-character rule rejects them.
        for (raw, want) in [
            ("-lead", NameError::BadFirstChar('-')),
            ("_lead", NameError::BadFirstChar('_')),
            ("Agent", NameError::BadFirstChar('A')),
        ] {
            assert_eq!(NodeName::new(raw).unwrap_err(), want, "{raw:?}");
        }
    }

    #[test]
    fn an_empty_name_is_empty_rather_than_a_char_error() {
        assert_eq!(NodeName::new("").unwrap_err(), NameError::Empty);
    }

    /// Uppercase is the one worth naming: node names are addresses, and two
    /// nodes differing only in case would be two addresses an operator reads
    /// as one.
    #[test]
    fn the_charset_is_lowercase_only_and_excludes_path_and_sql_punctuation() {
        for (raw, bad) in [
            ("agentA", 'A'),
            ("a b", ' '),
            ("a.b", '.'),
            ("a/b", '/'),
            ("a'b", '\''),
            ("a;b", ';'),
            ("a\"b", '"'),
            ("a\\b", '\\'),
            ("a\nb", '\n'),
            ("a\0b", '\0'),
            ("café", 'é'),
        ] {
            assert_eq!(
                NodeName::new(raw).unwrap_err(),
                NameError::BadChar(bad),
                "{raw:?} must be rejected"
            );
        }
    }

    #[test]
    fn length_is_capped_and_counted_in_characters_not_bytes() {
        assert!(NodeName::new("x".repeat(NAME_MAX_LEN)).is_ok());
        assert_eq!(
            NodeName::new("x".repeat(NAME_MAX_LEN + 1)).unwrap_err(),
            NameError::TooLong(NAME_MAX_LEN + 1)
        );
        // 64 three-byte chars: 192 bytes, but the operator is told 64.
        let wide = "☃".repeat(NAME_MAX_LEN + 1);
        assert_eq!(
            NodeName::new(&wide).unwrap_err(),
            NameError::TooLong(NAME_MAX_LEN + 1),
            "length must be reported in characters, not bytes"
        );
    }

    /// `user` is the `from_node` the engine stamps on UI-originated messages,
    /// so a node called `user` would make `wheel msg user ...` ambiguous about
    /// who is being addressed.
    #[test]
    fn reserved_names_are_refused() {
        for name in RESERVED_NAMES {
            assert_eq!(
                NodeName::new(*name).unwrap_err(),
                NameError::Reserved(name.to_string()),
                "{name:?} must stay reserved"
            );
        }
        // Only the exact word: these are ordinary names.
        for ok in ["users", "user-1", "user_data", "wheelhouse", "systemd"] {
            assert!(NodeName::new(ok).is_ok(), "{ok:?} should be allowed");
        }
    }

    /// `sqlite_table` interpolates straight into DDL, so the ONLY thing
    /// standing between a node name and SQL is this charset plus the '-' rule.
    #[test]
    fn a_table_name_is_only_produced_when_it_is_safe_to_interpolate() {
        assert_eq!(
            NodeName::new("notes").unwrap().sqlite_table().as_deref(),
            Some("t_notes")
        );
        assert_eq!(
            NodeName::new("my_notes_2")
                .unwrap()
                .sqlite_table()
                .as_deref(),
            Some("t_my_notes_2")
        );
        // A hyphen is a valid node name but a bare '-' in an identifier is
        // subtraction to sqlite, so it must refuse rather than emit it.
        assert_eq!(NodeName::new("my-notes").unwrap().sqlite_table(), None);

        // Whatever comes out contains nothing that could end an identifier.
        for raw in ["notes", "a", "z9_z"] {
            let t = NodeName::new(raw).unwrap().sqlite_table().unwrap();
            assert!(
                t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{t:?} is not a bare sqlite identifier"
            );
        }
    }

    /// The type claims an invalid name is unconstructable. serde is the way in
    /// that bypasses `new`, so it has to re-validate — otherwise any name on
    /// the wire lands in the database unchecked.
    #[test]
    fn deserialization_revalidates_rather_than_trusting_the_wire() {
        let good: NodeName = serde_json::from_str("\"researcher\"").unwrap();
        assert_eq!(good.as_str(), "researcher");
        assert_eq!(serde_json::to_string(&good).unwrap(), "\"researcher\"");

        for bad in ["\"\"", "\"User\"", "\"user\"", "\"a b\"", "\"a;drop\""] {
            assert!(
                serde_json::from_str::<NodeName>(bad).is_err(),
                "{bad} must not deserialize into a NodeName"
            );
        }
    }

    #[test]
    fn a_name_round_trips_through_every_conversion() {
        let n: NodeName = "researcher".parse().unwrap();
        assert_eq!(n.to_string(), "researcher");
        assert_eq!(n.as_ref() as &str, "researcher");
        assert_eq!(n.as_str(), "researcher");
        assert_eq!(n.clone().into_string(), "researcher");
        assert!("User".parse::<NodeName>().is_err());
    }

    /// Injected `# Context:` blocks are ordered by ctx node name so the
    /// preamble is stable and board-position-independent (§3). That ordering
    /// is this `Ord`, and it must be plain byte order.
    #[test]
    fn names_order_by_bytes_so_injected_context_is_stable() {
        let mut names: Vec<NodeName> = ["ctx-b", "ctx_a", "ctx-a", "0first", "zlast"]
            .iter()
            .map(|s| NodeName::new(*s).unwrap())
            .collect();
        names.sort();
        let got: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        assert_eq!(got, ["0first", "ctx-a", "ctx-b", "ctx_a", "zlast"]);
    }

    #[test]
    fn an_ident_allows_reserved_words_but_never_a_hyphen() {
        // These are terrible node names and perfectly good column names.
        for ok in ["user", "system", "engine", "wheel", "col_1"] {
            assert!(Ident::new(ok).is_ok(), "{ok:?} should be a valid column");
        }
        // A column name goes into DDL bare, so '-' is rejected outright rather
        // than handled later like it is for NodeName.
        assert_eq!(Ident::new("a-b").unwrap_err(), NameError::BadChar('-'));
        assert_eq!(Ident::new("").unwrap_err(), NameError::Empty);
        assert_eq!(Ident::new("A").unwrap_err(), NameError::BadFirstChar('A'));
        assert_eq!(Ident::new("a;b").unwrap_err(), NameError::BadChar(';'));
        assert_eq!(
            Ident::new("x".repeat(NAME_MAX_LEN + 1)).unwrap_err(),
            NameError::TooLong(NAME_MAX_LEN + 1)
        );
    }

    #[test]
    fn an_ident_round_trips_and_revalidates_on_the_wire() {
        let i = Ident::new("col_1").unwrap();
        assert_eq!(i.to_string(), "col_1");
        assert_eq!(i.as_str(), "col_1");
        assert_eq!(serde_json::to_string(&i).unwrap(), "\"col_1\"");
        assert_eq!(
            serde_json::from_str::<Ident>("\"col_1\"").unwrap().as_str(),
            "col_1"
        );
        assert!(serde_json::from_str::<Ident>("\"a-b\"").is_err());
    }

    #[test]
    fn the_error_messages_name_the_offending_character() {
        assert!(NameError::BadChar(';').to_string().contains("';'"));
        assert!(NameError::BadFirstChar('A').to_string().contains("'A'"));
        assert!(NameError::TooLong(64).to_string().contains("64"));
        assert!(NameError::Reserved("user".into())
            .to_string()
            .contains("user"));
    }

    /// PM ruling: a table node's name must already be a sqlite identifier, and
    /// is REFUSED rather than rewritten. Silently turning `table-1` into
    /// `table_1` would put the node at an address the operator did not choose,
    /// and every `wheel read table-1` afterwards would fail for a reason
    /// nothing explains.
    #[test]
    fn a_table_node_name_must_already_be_a_sqlite_identifier() {
        for ok in ["notes", "my_notes", "t2", "a"] {
            assert!(validate_table_name(ok).is_ok(), "{ok:?} should be allowed");
        }

        // Legal as a NODE name, illegal as a TABLE node — which is the whole
        // reason this is a separate rule rather than a tightening of the one
        // every node shares.
        for raw in ["my-notes", "a-b"] {
            assert!(NodeName::new(raw).is_ok(), "{raw:?} is a legal node name");
            assert_eq!(validate_table_name(raw), Err(NameError::TableBadChar('-')));
        }
        for raw in ["1notes", "0"] {
            assert!(NodeName::new(raw).is_ok(), "{raw:?} is a legal node name");
            assert!(matches!(
                validate_table_name(raw),
                Err(NameError::TableBadFirstChar(_))
            ));
        }

        // Everything the ordinary rule rejects is still rejected.
        assert_eq!(validate_table_name(""), Err(NameError::Empty));
        assert_eq!(
            validate_table_name("user"),
            Err(NameError::Reserved("user".into()))
        );
        assert_eq!(validate_table_name("A"), Err(NameError::BadFirstChar('A')));
    }

    #[test]
    fn the_table_name_error_says_what_to_do_instead() {
        let e = validate_table_name("my-notes").unwrap_err().to_string();
        assert!(e.contains("t_<name>"), "{e}");
        assert!(e.contains("'_'"), "the fix must be named: {e}");
        let e = validate_table_name("1notes").unwrap_err().to_string();
        assert!(e.contains("lowercase letter"), "{e}");
    }
}
