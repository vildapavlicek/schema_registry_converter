use std::collections::HashSet;

use avro_rs::schema::{Name, Schema, SchemaType as AvroSchemaType};
use avro_rs::types::{Record, ToAvro, Value};
use avro_rs::{to_avro_datum, to_value};
use serde::ser::Serialize;
use serde_json::{value, Map};

use crate::error::SRCError;
use crate::schema_registry_common::{get_payload, SchemaType, SuppliedSchema};

/// Because we need both the resulting schema, as have a way of posting the schema as json, we use
/// this struct so we keep them both together.
#[derive(Clone, Debug)]
pub(crate) struct AvroSchema {
    pub(crate) id: u32,
    pub(crate) raw: String,
    pub(crate) parsed: Schema,
}

#[derive(Debug, PartialEq)]
pub struct DecodeResult<'s> {
    pub name: Option<Name<'s>>,
    pub value: Value,
}

fn might_replace(
    val: value::Value,
    child: &value::Value,
    replace_values: &HashSet<String>,
) -> value::Value {
    match val {
        value::Value::Object(v) => replace_in_map(v, child, replace_values),
        value::Value::Array(v) => replace_in_array(&*v, child, replace_values),
        value::Value::String(s) if replace_values.contains(&*s) => child.clone(),
        p => p,
    }
}

fn replace_in_array(
    parent_array: &[value::Value],
    child: &value::Value,
    replace_values: &HashSet<String>,
) -> value::Value {
    value::Value::Array(
        parent_array
            .iter()
            .map(|v| might_replace(v.clone(), child, replace_values))
            .collect(),
    )
}

fn replace_in_map(
    parent_map: Map<String, value::Value>,
    child: &value::Value,
    replace_values: &HashSet<String>,
) -> value::Value {
    value::Value::Object(
        parent_map
            .iter()
            .map(|e| {
                (
                    e.0.clone(),
                    might_replace(e.1.clone(), child, replace_values),
                )
            })
            .collect(),
    )
}

pub(crate) fn replace_reference(parent: value::Value, child: value::Value) -> value::Value {
    let (name, namespace) = match &child {
        value::Value::Object(v) => (v["name"].as_str(), v["namespace"].as_str()),
        _ => return parent,
    };
    let mut replace_values: HashSet<String> = HashSet::new();
    match name {
        Some(v) => match namespace {
            Some(u) => {
                replace_values.insert(format!(".{}.{}", u, v));
                if parent["namespace"].as_str() == namespace {
                    replace_values.insert(String::from(v))
                } else {
                    true
                }
            }
            None => replace_values.insert(String::from(v)),
        },
        None => return parent,
    };
    match parent {
        value::Value::Object(v) => replace_in_map(v, &child, &replace_values),
        value::Value::Array(v) => replace_in_array(&*v, &child, &replace_values),
        p => p,
    }
}

fn to_bytes<T: Into<Value>>(avro_schema: &AvroSchema, record: T) -> Result<Vec<u8>, SRCError> {
    match to_avro_datum(&avro_schema.parsed, record) {
        Ok(v) => Ok(get_payload(avro_schema.id, v)),
        Err(e) => Err(SRCError::non_retryable_with_cause(
            e,
            "Could not get Avro bytes",
        )),
    }
}

/// Using the schema with a vector of values the values will be correctly deserialized according to
/// the avro specification.
pub(crate) fn values_to_bytes(
    avro_schema: &AvroSchema,
    values: Vec<(&'static str, Value)>,
) -> Result<Vec<u8>, SRCError> {
    let mut record = match Record::new(&avro_schema.parsed) {
        Some(v) => v,
        None => {
            return Err(SRCError::new(
                "Could not create record from schema",
                None,
                false,
            ));
        }
    };
    for value in values {
        record.put(value.0, value.1)
    }
    to_bytes(avro_schema, record)
}

/// Using the schema with an item implementing serialize the item will be correctly deserialized
/// according to the avro specification.
pub(crate) fn item_to_bytes(
    avro_schema: &AvroSchema,
    item: impl Serialize,
) -> Result<Vec<u8>, SRCError> {
    match to_value(item)
        .map_err(|e| SRCError::non_retryable_with_cause(e, "Could not transform to avro_rs value"))
        .map(|r| r.resolve(avro_schema.parsed.root()))
    {
        Ok(Ok(v)) => to_bytes(avro_schema, v),
        Ok(Err(e)) => Err(SRCError::non_retryable_with_cause(e, "Failed to resolve")),
        Err(e) => Err(e),
    }
}

pub(crate) fn get_name(schema: AvroSchemaType) -> Option<Name> {
    match schema {
        AvroSchemaType::Record(record_schema) => Some(record_schema.name()),
        _ => None,
    }
}

pub fn get_supplied_schema(schema: &Schema) -> Box<SuppliedSchema> {
    let name = match get_name(schema.root()) {
        None => None,
        Some(n) => match n.namespace() {
            None => Some(n.name().to_string()),
            Some(ns) => Some(format!("{}.{}", ns, n.name())),
        },
    };
    Box::from(SuppliedSchema {
        name,
        schema_type: SchemaType::Avro,
        schema: schema.canonical_form(),
        references: vec![],
    })
}

#[cfg(test)]
mod tests {
    use avro_rs::types::Value;
    use avro_rs::Schema;

    use crate::avro_common::{values_to_bytes, AvroSchema};
    use crate::error::SRCError;
    use test_utils::{Atype, ConfirmAccountCreation, Heartbeat};

    #[test]
    fn to_bytes_no_record() {
        let schema = AvroSchema {
            id: 5,
            raw: "".to_string(),
            parsed: Schema::Boolean,
        };
        let result = values_to_bytes(&schema, vec![("beat", Value::Long(3))]);
        assert_eq!(
            result,
            Err(SRCError::new(
                "Could not create record from schema",
                None,
                false,
            ))
        )
    }

    #[test]
    fn to_bytes_no_transfer_wrong() {
        let schema = AvroSchema {
            id: 5,
            raw: String::from(r#"{"type":"record","name":"Name","namespace":"nl.openweb.data","fields":[{"name":"name","type":"string","avro.java.string":"String"}]}"#),
            parsed: Schema::parse_str(r#"{"type":"record","name":"Name","namespace":"nl.openweb.data","fields":[{"name":"name","type":"string","avro.java.string":"String"}]}"#).unwrap(),
        };
        let result = values_to_bytes(&schema, vec![("beat", Value::Long(3))]);
        assert_eq!(
            result,
            Err(SRCError::new(
                "Could not get Avro bytes",
                Some(String::from(
                    "Validation error: value does not match schema"
                )),
                false,
            ))
        )
    }
    #[test]
    fn item_to_bytes_no_tranfer_wrong() {
        let schema = AvroSchema {
            id: 5,
            raw: String::from(
                r#"{"type":"record","name":"Name","namespace":"nl.openweb.data","fields":[{"name":"name","type":"string","avro.java.string":"String"}]}"#,
            ),
            parsed: Schema::parse_str(
                r#"{"type":"record","name":"Name","namespace":"nl.openweb.data","fields":[{"name":"name","type":"string","avro.java.string":"String"}]}"#,
            ).unwrap(),
        };
        let err = crate::avro_common::item_to_bytes(&schema, Heartbeat { beat: 3 }).unwrap_err();
        assert_eq!(
            err,
            SRCError::new(
                "Failed to resolve",
                Some(String::from(
                    "Schema resoulution error: missing field name in record"
                )),
                false,
            )
        )
    }

    #[test]
    fn item_to_bytes_still_broken() {
        let schema = AvroSchema {
            id: 6,
            raw: String::from(
                r#"{"type":"record","name":"ConfirmAccountCreation","namespace":"nl.openweb.data","fields":[{"name":"id","type":{"type":"fixed","name":"Uuid","size":16}},{"name":"a_type","type":{"type":"enum","name":"Atype","symbols":["AUTO","MANUAL"]}}]}"#,
            ),
            parsed: Schema::parse_str(
                r#"{"type":"record","name":"ConfirmAccountCreation","namespace":"nl.openweb.data","fields":[{"name":"id","type":{"type":"fixed","name":"Uuid","size":16}},{"name":"a_type","type":{"type":"enum","name":"Atype","symbols":["AUTO","MANUAL"]}}]}"#,
            ).unwrap(),
        };
        let item = ConfirmAccountCreation {
            id: [
                204, 240, 237, 74, 227, 188, 75, 46, 183, 163, 122, 214, 178, 72, 118, 162,
            ],
            a_type: Atype::Manual,
        };
        let result = crate::avro_common::item_to_bytes(&schema, item).unwrap_err();
        assert_eq!(
            result,
            SRCError::new(
                "Failed to resolve",
                Some(String::from("Schema resoulution error: String expected, got Array([Int(204), Int(240), Int(237), Int(74), Int(227), Int(188), Int(75), Int(46), Int(183), Int(163), Int(122), Int(214), Int(178), Int(72), Int(118), Int(162)])")),
                false,
            )
        )
    }
}
