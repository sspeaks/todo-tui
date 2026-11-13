//! The export format (design 7.4): JSON Lines of `todo.export.v1`
//! messages in their canonical proto3 JSON. Line 1 is the [`Header`]; each
//! later line is `{"type": "<record case>", "data": {…}}`.

use std::sync::LazyLock;

use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, MessageDescriptor, SerializeOptions,
};
use todo_core::FieldValue;
use todo_store::{
    EventRecord, EventValue, ExportRow, OpRecord, OutboxRecord, PatchRecord, SuggestionRecord,
};

use crate::convert::{
    ConvertError, EnumField as _, field_value_from_proto, timestamp_from_proto, timestamp_to_proto,
};
use crate::todo::export::v1::{self as ex, record::Record as R};
use crate::v1;

/// What `format` says on line 1.
pub const FORMAT: &str = "todo-tui-export";
/// The format version this build writes and reads.
pub const FORMAT_VERSION: u32 = 1;

pub use ex::Header;

static POOL: LazyLock<DescriptorPool> = LazyLock::new(|| {
    DescriptorPool::decode(crate::FILE_DESCRIPTOR_SET).expect("the descriptor set decodes")
});

fn descriptor(name: &str) -> MessageDescriptor {
    POOL.get_message_by_name(name).expect("a todo message")
}

/// Why a line could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LineError(pub String);

/// `message` as compact canonical proto3 JSON.
fn to_json<M: prost::Message>(name: &str, message: &M) -> serde_json::Value {
    let dynamic = DynamicMessage::decode(descriptor(name), message.encode_to_vec().as_slice())
        .expect("a message decodes as its own descriptor");
    dynamic
        .serialize_with_options(serde_json::value::Serializer, &SerializeOptions::new())
        .expect("proto3 JSON serializes")
}

/// `json` as message `name`. A field this build does not know (a later
/// minor's, design 4.3 rule 1) is skipped, not refused.
fn from_json<M: prost::Message + Default>(
    name: &str,
    json: serde_json::Value,
) -> Result<M, LineError> {
    let options = DeserializeOptions::new().deny_unknown_fields(false);
    let dynamic = DynamicMessage::deserialize_with_options(descriptor(name), json, &options)
        .map_err(|e| LineError(e.to_string()))?;
    M::decode(dynamic.encode_to_vec().as_slice()).map_err(|e| LineError(e.to_string()))
}

/// The header line.
pub fn header_line(header: &Header) -> String {
    to_json("todo.export.v1.Header", header).to_string()
}

/// Reads line 1, checking it is an export this build reads.
pub fn read_header(line: &str) -> Result<Header, LineError> {
    let json: serde_json::Value =
        serde_json::from_str(line).map_err(|e| LineError(format!("not JSON: {e}")))?;
    let header: Header = from_json("todo.export.v1.Header", json)?;
    if header.format != FORMAT {
        return Err(LineError(format!(
            "not a {FORMAT} file (format {:?})",
            header.format
        )));
    }
    if header.format_version != FORMAT_VERSION {
        return Err(LineError(format!(
            "format version {} is not {FORMAT_VERSION}, the one this todo reads",
            header.format_version
        )));
    }
    if header.schema_version > todo_store::SCHEMA_VERSION {
        return Err(LineError(format!(
            "from schema version {}, newer than this todo's ({})",
            header.schema_version,
            todo_store::SCHEMA_VERSION
        )));
    }
    Ok(header)
}

/// One record line.
pub fn record_line(row: &ExportRow) -> String {
    let record = record_of(row);
    let json = to_json("todo.export.v1.Record", &record);
    // {"item": {…}} → {"type": "item", "data": {…}}: one key, the case.
    let (kind, data) = json
        .as_object()
        .and_then(|o| o.iter().next())
        .map(|(k, v)| (k.clone(), v.clone()))
        .expect("a record has its case set");
    // "type" first, as design 7.4 shows the line (serde_json would sort it).
    format!(
        "{{\"type\":{},\"data\":{}}}",
        serde_json::Value::String(snake(&kind)),
        data
    )
}

/// Reads a record line.
pub fn read_record(line: &str) -> Result<ExportRow, LineError> {
    let json: serde_json::Value =
        serde_json::from_str(line).map_err(|e| LineError(format!("not JSON: {e}")))?;
    let obj = json
        .as_object()
        .ok_or_else(|| LineError("not a record".to_owned()))?;
    let kind = obj
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| LineError("a record without a type".to_owned()))?;
    if !RECORD_TYPES.contains(&kind) {
        return Err(LineError(format!("unknown record type {kind:?}")));
    }
    let data = obj
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let record: ex::Record = from_json(
        "todo.export.v1.Record",
        serde_json::json!({ camel(kind): data }),
    )?;
    row_of(&record).map_err(|e| LineError(e.to_string()))
}

const RECORD_TYPES: [&str; 10] = [
    "person",
    "item",
    "relation",
    "view",
    "view_seed",
    "op",
    "event",
    "sync_cursor",
    "outbox_op",
    "suggestion",
];

fn snake(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn record_of(row: &ExportRow) -> ex::Record {
    let record = match row {
        ExportRow::Person(p) => R::Person(v1::Person::from(&**p)),
        ExportRow::Item(item) => {
            let mut p = v1::Item::from(&**item);
            // Computed when read, so not part of the record.
            p.status = 0;
            p.effective_priority = 0;
            p.is_overdue = false;
            p.subtasks_total = 0;
            p.subtasks_done = 0;
            p.parent_title = None;
            p.relations.clear();
            R::Item(p)
        }
        ExportRow::Relation {
            relation,
            created_at,
        } => R::Relation(ex::RelationRecord {
            relation: Some(relation.into()),
            created_at: Some(timestamp_to_proto(*created_at)),
        }),
        ExportRow::View(view) => {
            let mut p = v1::View::from(&**view);
            p.query = String::new();
            R::View(p)
        }
        ExportRow::ViewSeed { seed, seeded_at } => R::ViewSeed(ex::ViewSeed {
            seed: seed.clone(),
            seeded_at: Some(timestamp_to_proto(*seeded_at)),
        }),
        ExportRow::SyncCursor {
            instance_id,
            scope,
            cursor_at,
        } => R::SyncCursor(ex::SyncCursor {
            instance_id: instance_id.clone(),
            scope: scope.clone(),
            cursor_at: Some(timestamp_to_proto(*cursor_at)),
        }),
        ExportRow::Outbox(o) => R::OutboxOp(ex::OutboxOp {
            id: o.id.clone(),
            item_id: o.item_id.clone(),
            instance_id: o.instance_id.clone(),
            op_id: o.op_id.clone(),
            kind: o.kind.clone(),
            field: o.field,
            base_json: o.base_json.clone(),
            new_json: o.new_json.clone(),
            sent_json: o.sent_json.clone(),
            state: o.state.clone(),
            attempts: o.attempts,
            next_attempt_at: o.next_attempt_at.map(timestamp_to_proto),
            last_error: o.last_error.clone(),
            created_at: Some(timestamp_to_proto(o.created_at)),
            updated_at: Some(timestamp_to_proto(o.updated_at)),
        }),
        ExportRow::Suggestion(s) => R::Suggestion(ex::SuggestionRecord {
            id: s.id.clone(),
            item_id: s.item_id.clone(),
            kind: s.kind.clone(),
            source: s.source.clone(),
            rationale: s.rationale.clone(),
            error: s.error.clone(),
            state: s.state.clone(),
            created_at: Some(timestamp_to_proto(s.created_at)),
            updated_at: Some(timestamp_to_proto(s.updated_at)),
            patches: s
                .patches
                .iter()
                .map(|p| ex::SuggestionPatchRecord {
                    id: p.id.clone(),
                    field: p.field,
                    expected_old_json: p.expected_old_json.clone(),
                    new_json: p.new_json.clone(),
                    state: p.state.clone(),
                })
                .collect(),
        }),
        ExportRow::Op(op) => R::Op(ex::Op {
            id: op.id.clone(),
            kind: op.kind.clone(),
            actor: op.actor.clone(),
            at: Some(timestamp_to_proto(op.at)),
            request_id: op.request_id.clone(),
            revision: op.revision,
            seq: op.seq,
            undo_state: op.undo_state.clone(),
            entry_op: op.entry_op.clone(),
            summary: op.summary.clone(),
            report: op.report.clone(),
        }),
        ExportRow::Event(e) => {
            let value = |v: &Option<EventValue>| {
                v.as_ref().map(|v| ex::EventValue {
                    value: Some(match v {
                        EventValue::Field(f) => ex::event_value::Value::FieldValue(f.into()),
                        EventValue::Staleness(s) => {
                            ex::event_value::Value::FieldValue((&FieldValue::Staleness(*s)).into())
                        }
                        EventValue::Relation(r) => ex::event_value::Value::Relation(r.into()),
                        EventValue::Title(t) => ex::event_value::Value::Title(t.clone()),
                    }),
                })
            };
            R::Event(ex::Event {
                id: e.id.clone(),
                op_id: e.op_id.clone(),
                item_id: e.item_id.clone(),
                at: Some(timestamp_to_proto(e.at)),
                actor: e.actor.clone(),
                kind: e.kind.clone(),
                field: e.field.map(|f| f.to_i32()),
                old: value(&e.old),
                new: value(&e.new),
                ord: e.ord,
            })
        }
    };
    ex::Record {
        record: Some(record),
    }
}

fn row_of(record: &ex::Record) -> Result<ExportRow, ConvertError> {
    let ts = |field: &'static str, t: &Option<prost_types::Timestamp>| {
        t.as_ref()
            .ok_or_else(|| ConvertError::Missing(field.into()))
            .and_then(|t| timestamp_from_proto(field, t))
    };
    Ok(
        match record
            .record
            .as_ref()
            .ok_or_else(|| ConvertError::Missing("record".into()))?
        {
            R::Item(p) => {
                let mut item = todo_core::Item::try_from(p)?;
                item.derived.is_overdue = false;
                ExportRow::Item(Box::new(item))
            }
            R::Relation(r) => ExportRow::Relation {
                relation: r
                    .relation
                    .as_ref()
                    .ok_or_else(|| ConvertError::Missing("relation".into()))?
                    .try_into()?,
                created_at: ts("created_at", &r.created_at)?,
            },
            R::View(v) => ExportRow::View(Box::new(todo_core::view::View::try_from(v)?)),
            R::ViewSeed(s) => ExportRow::ViewSeed {
                seed: s.seed.clone(),
                seeded_at: ts("seeded_at", &s.seeded_at)?,
            },
            R::SyncCursor(c) => ExportRow::SyncCursor {
                instance_id: c.instance_id.clone(),
                scope: c.scope.clone(),
                cursor_at: ts("cursor_at", &c.cursor_at)?,
            },
            R::OutboxOp(o) => ExportRow::Outbox(Box::new(OutboxRecord {
                id: o.id.clone(),
                item_id: o.item_id.clone(),
                instance_id: o.instance_id.clone(),
                op_id: o.op_id.clone(),
                kind: o.kind.clone(),
                field: o.field,
                base_json: o.base_json.clone(),
                new_json: o.new_json.clone(),
                sent_json: o.sent_json.clone(),
                state: o.state.clone(),
                attempts: o.attempts,
                next_attempt_at: o
                    .next_attempt_at
                    .as_ref()
                    .map(|t| timestamp_from_proto("next_attempt_at", t))
                    .transpose()?,
                last_error: o.last_error.clone(),
                created_at: ts("created_at", &o.created_at)?,
                updated_at: ts("updated_at", &o.updated_at)?,
            })),
            R::Person(p) => ExportRow::Person(Box::new(todo_core::Person::try_from(p)?)),
            R::Suggestion(s) => ExportRow::Suggestion(Box::new(SuggestionRecord {
                id: s.id.clone(),
                item_id: s.item_id.clone(),
                kind: s.kind.clone(),
                source: s.source.clone(),
                rationale: s.rationale.clone(),
                error: s.error.clone(),
                state: s.state.clone(),
                created_at: ts("created_at", &s.created_at)?,
                updated_at: ts("updated_at", &s.updated_at)?,
                patches: s
                    .patches
                    .iter()
                    .map(|p| PatchRecord {
                        id: p.id.clone(),
                        field: p.field,
                        expected_old_json: p.expected_old_json.clone(),
                        new_json: p.new_json.clone(),
                        state: p.state.clone(),
                    })
                    .collect(),
            })),
            R::Op(op) => ExportRow::Op(Box::new(OpRecord {
                id: op.id.clone(),
                kind: op.kind.clone(),
                actor: op.actor.clone(),
                at: ts("at", &op.at)?,
                request_id: op.request_id.clone(),
                revision: op.revision,
                seq: op.seq,
                undo_state: op.undo_state.clone(),
                entry_op: op.entry_op.clone(),
                summary: op.summary.clone(),
                report: op.report.clone(),
            })),
            R::Event(e) => {
                let value = |field: &'static str,
                             v: &Option<ex::EventValue>|
                 -> Result<Option<EventValue>, ConvertError> {
                    let Some(v) = v else { return Ok(None) };
                    Ok(Some(
                        match v
                            .value
                            .as_ref()
                            .ok_or_else(|| ConvertError::Missing(field.into()))?
                        {
                            ex::event_value::Value::FieldValue(f) => {
                                match field_value_from_proto(field, f)? {
                                    FieldValue::Staleness(s) if e.kind == "derived" => {
                                        EventValue::Staleness(s)
                                    }
                                    other => EventValue::Field(other),
                                }
                            }
                            ex::event_value::Value::Relation(r) => {
                                EventValue::Relation(r.try_into()?)
                            }
                            ex::event_value::Value::Title(t) => EventValue::Title(t.clone()),
                        },
                    ))
                };
                ExportRow::Event(Box::new(EventRecord {
                    id: e.id.clone(),
                    op_id: e.op_id.clone(),
                    item_id: e.item_id.clone(),
                    at: ts("at", &e.at)?,
                    actor: e.actor.clone(),
                    kind: e.kind.clone(),
                    field: e
                        .field
                        .map(|n| todo_core::ItemField::from_i32("field", n))
                        .transpose()?,
                    old: value("old", &e.old)?,
                    new: value("new", &e.new)?,
                    ord: e.ord,
                }))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> ExportRow {
        ExportRow::ViewSeed {
            seed: "inbox".into(),
            seeded_at: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        }
    }

    #[test]
    fn a_line_says_its_type_first_and_reads_back() {
        let line = record_line(&seed());
        assert_eq!(
            line,
            r#"{"type":"view_seed","data":{"seed":"inbox","seededAt":"2023-11-14T22:13:20Z"}}"#
        );
        assert_eq!(read_record(&line).unwrap(), seed());
    }

    #[test]
    fn fields_a_later_minor_adds_are_skipped() {
        let line = r#"{"type":"view_seed","data":{"seed":"inbox","seededAt":"2023-11-14T22:13:20Z","futureField":7}}"#;
        assert_eq!(read_record(line).unwrap(), seed());
        let header =
            r#"{"format":"todo-tui-export","formatVersion":1,"schemaVersion":1,"futureField":"x"}"#;
        assert_eq!(read_header(header).unwrap().format_version, 1);
    }

    #[test]
    fn a_header_this_build_cannot_read_is_refused() {
        assert!(read_header(r#"{"format":"other","formatVersion":1}"#).is_err());
        assert!(read_header(r#"{"format":"todo-tui-export","formatVersion":2}"#).is_err());
        assert!(
            read_header(r#"{"format":"todo-tui-export","formatVersion":1,"schemaVersion":999}"#)
                .is_err()
        );
        assert!(read_record(r#"{"type":"frob","data":{}}"#).is_err());
    }
}
