use std::collections::BTreeSet;

use miette::{Result, miette};
use serde_json::{Map, Value, json};

mod field;
pub use field::{Nullable, Presence};

const ALLOWED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
];

const FORBIDDEN_SCHEMA_KEYS: &[&str] = &[
    "$defs",
    "$ref",
    "definitions",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    "dependentRequired",
    "dependentSchemas",
    "prefixItems",
    "default",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minimum",
    "maximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "contains",
];

const SIMPLE_TYPES: &[&str] = &[
    "string", "integer", "number", "boolean", "object", "array", "null",
];

pub trait ModelSchema {
    fn model_schema() -> Value;
}

pub fn model_schema_for<T: ModelSchema>() -> Value {
    model_schema(T::model_schema())
}

pub fn model_schema(schema: Value) -> Value {
    validate_model_facing_schema(&schema).expect("model-facing schema must be valid");
    schema
}

pub fn validate_model_facing_schema(schema: &Value) -> Result<()> {
    let mut errors = Vec::new();
    validate_schema_at(schema, "$", true, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(miette!(
            "invalid model-facing JSON schema:\n{}",
            errors.join("\n")
        ))
    }
}

/// Strict upstream decoding requires every object property to be required.
/// Never rewrite optional members into nullable members: that loses presence.
pub fn supports_strict_schema(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let Some(required) = object.get("required").and_then(Value::as_array) else {
            return false;
        };
        if object.get("additionalProperties") != Some(&Value::Bool(false))
            || required.len() != properties.len()
            || !properties
                .keys()
                .all(|key| required.iter().any(|name| name.as_str() == Some(key)))
            || !properties.values().all(supports_strict_schema)
        {
            return false;
        }
    }
    object.get("items").is_none_or(supports_strict_schema)
}

pub fn validate_value_against_schema(value: &Value, schema: &Value, label: &str) -> Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| miette!("{label} schema must be a JSON object"))?;

    if let Some(enum_values) = object.get("enum") {
        let enum_values = enum_values
            .as_array()
            .ok_or_else(|| miette!("{label}.enum must be an array"))?;
        if !enum_values.iter().any(|candidate| candidate == value) {
            return Err(miette!("{label} must match one of the allowed enum values"));
        }
    }

    let type_names = value_schema_type_names(object, label)?;
    if value.is_null() {
        if type_names.iter().any(|type_name| type_name == "null") {
            return Ok(());
        }
        return Err(miette!("{label} must not be null"));
    }

    let mut matched = false;
    for type_name in &type_names {
        match type_name.as_str() {
            "object" if value.is_object() => {
                validate_object_instance(value, object, label)?;
                matched = true;
            }
            "array" if value.is_array() => {
                validate_array_instance(value, object, label)?;
                matched = true;
            }
            "string" if value.is_string() => matched = true,
            "integer" if value.as_i64().is_some() || value.as_u64().is_some() => matched = true,
            "number" if value.is_number() => matched = true,
            "boolean" if value.is_boolean() => matched = true,
            _ => {}
        }
    }
    if matched {
        Ok(())
    } else if type_names.len() == 1 {
        Err(miette!("{label} must be a {}", type_names[0]))
    } else {
        Err(miette!(
            "{label} must match schema type {}",
            type_names.join(" or ")
        ))
    }
}

fn value_schema_type_names(object: &Map<String, Value>, label: &str) -> Result<Vec<String>> {
    let type_value = object
        .get("type")
        .ok_or_else(|| miette!("{label}.type is required"))?;
    match type_value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .map(ToString::to_string)
                    .ok_or_else(|| miette!("{label}.type[{index}] must be a string"))
            })
            .collect(),
        _ => Err(miette!("{label}.type must be a string or string array")),
    }
}

fn validate_object_instance(value: &Value, schema: &Map<String, Value>, label: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| miette!("{label} must be an object"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| miette!("{label}.properties must be an object"))?;
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| miette!("{label}.required must be an array"))?;

    for field in required {
        let field = field
            .as_str()
            .ok_or_else(|| miette!("{label}.required entries must be strings"))?;
        if !object.contains_key(field) {
            return Err(miette!("{label}.{field} is required"));
        }
    }
    for (key, field_value) in object {
        let Some(field_schema) = properties.get(key) else {
            return Err(miette!("{label}.{key} is not allowed"));
        };
        validate_value_against_schema(field_value, field_schema, &format!("{label}.{key}"))?;
    }
    Ok(())
}

fn validate_array_instance(value: &Value, schema: &Map<String, Value>, label: &str) -> Result<()> {
    let items = value
        .as_array()
        .ok_or_else(|| miette!("{label} must be an array"))?;
    let item_schema = schema
        .get("items")
        .ok_or_else(|| miette!("{label}.items is required"))?;
    for (index, item) in items.iter().enumerate() {
        validate_value_against_schema(item, item_schema, &format!("{label}[{index}]"))?;
    }
    Ok(())
}

pub fn string_schema() -> Value {
    json!({ "type": "string" })
}

pub fn integer_schema() -> Value {
    json!({ "type": "integer" })
}

pub fn number_schema() -> Value {
    json!({ "type": "number" })
}

pub fn boolean_schema() -> Value {
    json!({ "type": "boolean" })
}

pub fn array_schema(items: &Value) -> Value {
    json!({
        "type": "array",
        "items": items,
    })
}

pub fn string_enum_schema(values: &[&str]) -> Value {
    json!({
        "type": "string",
        "enum": values,
    })
}

pub fn nullable_schema(mut schema: Value) -> Value {
    let Some(object) = schema.as_object_mut() else {
        return schema;
    };

    let type_value = object
        .remove("type")
        .unwrap_or_else(|| Value::String("object".to_string()));
    let mut types = match type_value {
        Value::String(value) => vec![Value::String(value)],
        Value::Array(values) => values,
        other => vec![other],
    };
    if !types.iter().any(|value| value == "null") {
        types.push(Value::String("null".to_string()));
    }
    object.insert("type".to_string(), Value::Array(types));

    if let Some(Value::Array(values)) = object.get_mut("enum")
        && !values.iter().any(Value::is_null)
    {
        values.push(Value::Null);
    }

    schema
}

fn validate_schema_at(schema: &Value, path: &str, root: bool, errors: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        errors.push(format!("{path}: schema must be a JSON object"));
        return;
    };

    for key in object.keys() {
        if FORBIDDEN_SCHEMA_KEYS.contains(&key.as_str()) {
            errors.push(format!("{path}: forbidden schema keyword `{key}`"));
        } else if !ALLOWED_SCHEMA_KEYS.contains(&key.as_str()) {
            errors.push(format!("{path}: unsupported schema keyword `{key}`"));
        }
    }

    let type_names = parse_type_names(object.get("type"), path, errors);
    if root && !type_names.iter().any(|name| name == "object") {
        errors.push(format!("{path}: root schema must be an object schema"));
    }
    validate_type_union(&type_names, path, errors);

    if let Some(description) = object.get("description")
        && !description.is_string()
    {
        errors.push(format!("{path}.description: must be a string"));
    }

    if let Some(enum_values) = object.get("enum") {
        validate_enum(enum_values, &type_names, path, errors);
    }

    if type_names.iter().any(|name| name == "object") || object.contains_key("properties") {
        validate_object_schema(object, path, errors);
    }

    if type_names.iter().any(|name| name == "array") || object.contains_key("items") {
        validate_array_schema(object, path, errors);
    }

    if let Some(additional_properties) = object.get("additionalProperties")
        && additional_properties != &Value::Bool(false)
    {
        errors.push(format!(
            "{path}.additionalProperties: must be exactly false"
        ));
    }
}

fn parse_type_names(
    type_value: Option<&Value>,
    path: &str,
    errors: &mut Vec<String>,
) -> Vec<String> {
    let Some(type_value) = type_value else {
        errors.push(format!("{path}.type: missing type"));
        return Vec::new();
    };
    match type_value {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) => values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                value.as_str().map_or_else(
                    || {
                        errors.push(format!("{path}.type[{index}]: must be a string"));
                        None
                    },
                    |value| Some(value.to_string()),
                )
            })
            .collect(),
        _ => {
            errors.push(format!("{path}.type: must be a string or string array"));
            Vec::new()
        }
    }
}

fn validate_type_union(type_names: &[String], path: &str, errors: &mut Vec<String>) {
    if type_names.is_empty() {
        return;
    }
    let mut seen = BTreeSet::new();
    for type_name in type_names {
        if !SIMPLE_TYPES.contains(&type_name.as_str()) {
            errors.push(format!("{path}.type: unsupported type `{type_name}`"));
        }
        if !seen.insert(type_name.as_str()) {
            errors.push(format!("{path}.type: duplicate type `{type_name}`"));
        }
    }
    if type_names.len() > 2
        || (type_names.len() == 2 && !type_names.iter().any(|name| name == "null"))
    {
        errors.push(format!(
            "{path}.type: only nullable unions with one non-null type are supported"
        ));
    }
}

fn validate_enum(enum_values: &Value, type_names: &[String], path: &str, errors: &mut Vec<String>) {
    let Some(values) = enum_values.as_array() else {
        errors.push(format!("{path}.enum: must be an array"));
        return;
    };
    if values.is_empty() {
        errors.push(format!("{path}.enum: must not be empty"));
    }
    for (index, value) in values.iter().enumerate() {
        match value {
            Value::String(_) => {
                if !type_names.iter().any(|name| name == "string") {
                    errors.push(format!(
                        "{path}.enum[{index}]: string value without string type"
                    ));
                }
            }
            Value::Null => {
                if !type_names.iter().any(|name| name == "null") {
                    errors.push(format!(
                        "{path}.enum[{index}]: null value without null type"
                    ));
                }
            }
            _ => errors.push(format!(
                "{path}.enum[{index}]: only string and null values are supported"
            )),
        }
    }
}

fn validate_object_schema(object: &Map<String, Value>, path: &str, errors: &mut Vec<String>) {
    let Some(properties) = object.get("properties") else {
        errors.push(format!("{path}.properties: missing properties object"));
        return;
    };
    let Some(properties) = properties.as_object() else {
        errors.push(format!("{path}.properties: must be an object"));
        return;
    };

    match object.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(_) => errors.push(format!(
            "{path}.additionalProperties: must be exactly false"
        )),
        None => errors.push(format!(
            "{path}.additionalProperties: missing additionalProperties=false"
        )),
    }

    let Some(required) = object.get("required") else {
        errors.push(format!("{path}.required: missing required array"));
        return;
    };
    let Some(required) = required.as_array() else {
        errors.push(format!("{path}.required: must be an array"));
        return;
    };
    let mut required_names = BTreeSet::new();
    for (index, item) in required.iter().enumerate() {
        if let Some(name) = item.as_str() {
            if !required_names.insert(name) {
                errors.push(format!(
                    "{path}.required[{index}]: duplicate property `{name}`"
                ));
            }
            if !properties.contains_key(name) {
                errors.push(format!(
                    "{path}.required[{index}]: unknown property `{name}`"
                ));
            }
        } else {
            errors.push(format!("{path}.required[{index}]: must be a string"));
        }
    }

    for (name, property_schema) in properties {
        validate_schema_at(
            property_schema,
            &format!("{path}.properties.{name}"),
            false,
            errors,
        );
    }
}

fn validate_array_schema(object: &Map<String, Value>, path: &str, errors: &mut Vec<String>) {
    let Some(items) = object.get("items") else {
        errors.push(format!("{path}.items: missing homogeneous item schema"));
        return;
    };
    validate_schema_at(items, &format!("{path}.items"), false, errors);
}

#[cfg(test)]
mod tests {
    use super::{model_schema, validate_model_facing_schema};
    use serde_json::json;

    #[test]
    fn validator_rejects_provider_normalization_cases() {
        let schema = json!({
            "type": "object",
            "properties": {
                "case": {
                    "$ref": "#/$defs/SearchCase",
                    "default": "smart"
                }
            },
            "required": ["case"],
            "additionalProperties": false,
            "$defs": {
                "SearchCase": {
                    "type": "string",
                    "enum": ["sensitive", "insensitive", "smart"]
                }
            }
        });

        let err = validate_model_facing_schema(&schema)
            .unwrap_err()
            .to_string();

        assert!(err.contains("forbidden schema keyword `$defs`"), "{err}");
        assert!(err.contains("forbidden schema keyword `$ref`"), "{err}");
        assert!(err.contains("forbidden schema keyword `default`"), "{err}");
    }

    #[test]
    fn optional_presence_and_nullability_are_validated_independently() {
        let schema = json!({
            "type": "object",
            "properties": {
                "required_text": { "type": "string" },
                "optional_text": { "type": ["string", "null"] },
                "optional_non_null": { "type": "string" }
            },
            "required": ["required_text"],
            "additionalProperties": false
        });

        validate_model_facing_schema(&schema).unwrap();
        for input in [
            json!({"required_text":"yes"}),
            json!({"required_text":"yes","optional_text":null}),
            json!({"required_text":"yes","optional_non_null":"value"}),
        ] {
            super::validate_value_against_schema(&input, &schema, "args").unwrap();
        }
        for input in [
            json!({}),
            json!({"required_text":null}),
            json!({"required_text":"yes","optional_non_null":null}),
            json!({"required_text":"yes","extra":false}),
        ] {
            assert!(super::validate_value_against_schema(&input, &schema, "args").is_err());
        }
        for required in [
            json!(["required_text", "unknown"]),
            json!(["required_text", "required_text"]),
        ] {
            let mut invalid = schema.clone();
            invalid["required"] = required;
            assert!(validate_model_facing_schema(&invalid).is_err());
        }
    }

    #[daat_locus_macros::model_schema]
    #[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Child {
        text: String,
        note: Option<String>,
    }

    #[daat_locus_macros::model_schema]
    #[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct FieldContract {
        text: String,
        maybe: Option<String>,
        clear: super::Nullable<String>,
        non_null: super::Presence<String>,
        update: super::Presence<super::Nullable<String>>,
        #[serde(default)]
        refresh: bool,
        nested: Option<Vec<Child>>,
    }

    #[test]
    fn model_fields_preserve_omission_explicit_null_and_values() {
        use super::{Nullable, Presence, model_schema_for, validate_value_against_schema};
        let schema = model_schema_for::<FieldContract>();
        let base = json!({"text":"request","clear":null});
        validate_value_against_schema(&base, &schema, "args").unwrap();
        let parsed: FieldContract = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(parsed.maybe, None);
        assert_eq!(parsed.clear, Nullable::Null);
        assert_eq!(parsed.non_null, Presence::Missing);
        assert_eq!(parsed.update, Presence::Missing);
        assert!(!parsed.refresh);
        let encoded = serde_json::to_value(&parsed).unwrap();
        assert!(encoded.get("update").is_none());
        assert!(encoded.get("non_null").is_none());
        let descriptor = schemars::schema_for!(FieldContract).to_value();
        let required = descriptor["required"].as_array().unwrap();
        assert!(required.iter().any(|field| field == "clear"));
        assert!(!required.iter().any(|field| field == "update"));

        for (update, expected) in [
            (json!(null), Presence::Present(Nullable::Null)),
            (
                json!("replacement"),
                Presence::Present(Nullable::Value("replacement".to_string())),
            ),
        ] {
            let mut input = base.clone();
            input["update"] = update.clone();
            input["non_null"] = json!("supplied");
            input["nested"] = json!([{"text":"child"}]);
            validate_value_against_schema(&input, &schema, "args").unwrap();
            let parsed: FieldContract = serde_json::from_value(input).unwrap();
            assert_eq!(parsed.update, expected);
            assert_eq!(parsed.non_null, Presence::Present("supplied".to_string()));
            assert_eq!(parsed.nested.as_ref().unwrap()[0].note, None);
            assert_eq!(serde_json::to_value(parsed).unwrap()["update"], update);
        }
        for input in [
            json!({"text":"request"}),
            json!({"text":"request","clear":null,"non_null":null}),
            json!({"text":"request","clear":null,"refresh":null}),
            json!({"text":"request","clear":null,"nested":[{}]}),
            json!({"text":"request","clear":12}),
        ] {
            assert!(
                validate_value_against_schema(&input, &schema, "args").is_err(),
                "{input}"
            );
            assert!(
                serde_json::from_value::<FieldContract>(input.clone()).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn strict_decoding_requires_nested_array_members_to_be_required() {
        use super::supports_strict_schema;
        let child = json!({"type":"object","properties":{"optional":{"type":"string"}},"required":[],"additionalProperties":false});
        let nested = json!({"type":"object","properties":{"rows":{"type":"array","items":child}},"required":["rows"],"additionalProperties":false});
        assert!(!supports_strict_schema(&nested));
        let mut required = nested.clone();
        required["properties"]["rows"]["items"]["required"] = json!(["optional"]);
        assert!(supports_strict_schema(&required));
    }

    #[test]
    fn validator_rejects_validation_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string" }
                }
            },
            "required": ["items"],
            "additionalProperties": false
        });

        let err = validate_model_facing_schema(&schema)
            .unwrap_err()
            .to_string();

        assert!(err.contains("forbidden schema keyword `minItems`"), "{err}");
    }

    #[test]
    fn model_schema_panics_for_invalid_schemas() {
        let result = std::panic::catch_unwind(|| {
            model_schema(json!({
                "type": "object",
                "properties": {},
            }));
        });

        assert!(result.is_err());
    }
}
