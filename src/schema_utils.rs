use std::collections::BTreeSet;

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use serde_json::{Map, Value, json};

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

pub fn object_schema<const N: usize>(properties: [(&'static str, Value); N]) -> Value {
    let mut property_map = Map::new();
    let mut required = Vec::new();
    for (name, schema) in properties {
        property_map.insert(name.to_string(), schema);
        required.push(Value::String(name.to_string()));
    }
    model_schema(json!({
        "type": "object",
        "properties": property_map,
        "required": required,
        "additionalProperties": false,
    }))
}

pub fn string_schema() -> Value {
    json!({ "type": "string" })
}

pub fn nullable_string_schema() -> Value {
    nullable_schema(string_schema())
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

pub fn array_schema(items: Value) -> Value {
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

#[allow(dead_code)]
#[model_schema]
#[derive(serde::Serialize, serde::Deserialize)]
struct StructuredEditArgsSchema {
    edits: Vec<StructuredEditSchema>,
}

#[allow(dead_code)]
#[model_schema]
#[derive(serde::Serialize, serde::Deserialize)]
struct StructuredEditSchema {
    path: String,
    op: StructuredEditOpSchema,
    start: String,
    end: Option<String>,
    content: Option<String>,
}

#[allow(dead_code)]
#[model_schema]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum StructuredEditOpSchema {
    Replace,
    Append,
    Prepend,
}

pub fn structured_edit_args_schema() -> Value {
    model_schema_for::<StructuredEditArgsSchema>()
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
                if let Some(value) = value.as_str() {
                    Some(value.to_string())
                } else {
                    errors.push(format!("{path}.type[{index}]: must be a string"));
                    None
                }
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
            required_names.insert(name);
        } else {
            errors.push(format!("{path}.required[{index}]: must be a string"));
        }
    }
    let property_names = properties
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if required_names != property_names {
        errors.push(format!(
            "{path}.required: must exactly match properties; required={required_names:?} properties={property_names:?}"
        ));
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
    use super::{
        boolean_schema, model_schema, nullable_string_schema, object_schema,
        structured_edit_args_schema, validate_model_facing_schema,
    };
    use serde_json::{Value, json};

    fn contains_key(value: &Value, needle: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(needle)
                    || object.values().any(|value| contains_key(value, needle))
            }
            Value::Array(values) => values.iter().any(|value| contains_key(value, needle)),
            _ => false,
        }
    }

    #[test]
    fn object_schema_requires_every_property_and_forbids_extra_properties() {
        let schema = object_schema([
            ("text", nullable_string_schema()),
            ("enabled", boolean_schema()),
        ]);

        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["text", "enabled"]));
        validate_model_facing_schema(&schema).unwrap();
    }

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
    fn validator_rejects_optional_properties_by_omission() {
        let schema = json!({
            "type": "object",
            "properties": {
                "required_text": { "type": "string" },
                "optional_text": { "type": ["string", "null"] }
            },
            "required": ["required_text"],
            "additionalProperties": false
        });

        let err = validate_model_facing_schema(&schema)
            .unwrap_err()
            .to_string();

        assert!(err.contains("must exactly match properties"), "{err}");
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
    fn structured_edit_schema_uses_portable_nullable_fields() {
        let schema = structured_edit_args_schema();

        validate_model_facing_schema(&schema).unwrap();
        for key in ["oneOf", "anyOf", "allOf", "minItems"] {
            assert!(!contains_key(&schema, key), "{schema:#}");
        }
        assert_eq!(
            schema["properties"]["edits"]["items"]["properties"]["content"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            schema["properties"]["edits"]["items"]["properties"]["end"]["type"],
            json!(["string", "null"])
        );
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
