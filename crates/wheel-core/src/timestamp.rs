//! RFC3339 UTC timestamps (ARCHITECTURE.md §2: "Time: RFC3339 UTC").

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// An instant, always serialized as an RFC3339 UTC string, e.g.
/// `2026-09-05T00:21:00Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }
    pub fn into_inner(self) -> OffsetDateTime {
        self.0
    }
    pub fn to_rfc3339(self) -> String {
        self.0
            .to_offset(time::UtcOffset::UTC)
            .format(&Rfc3339)
            .expect("RFC3339 formatting of a UTC timestamp cannot fail")
    }
    pub fn parse_rfc3339(s: &str) -> Result<Self, time::error::Parse> {
        Ok(Self(OffsetDateTime::parse(s, &Rfc3339)?))
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(t: OffsetDateTime) -> Self {
        Self(t)
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse_rfc3339(&raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Timestamp {
    fn schema_name() -> String {
        "Timestamp".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, Metadata, SchemaObject};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            format: Some("date-time".into()),
            metadata: Some(Box::new(Metadata {
                description: Some("RFC3339 UTC timestamp, e.g. 2026-09-05T00:21:00Z".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}
