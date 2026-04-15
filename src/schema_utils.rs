use serde_json::{Map, Value, json};

pub fn normalize_openai_json_schema(mut schema: Value) -> Value {
    normalize_openai_json_schema_in_place(&mut schema);
    schema
}

pub fn normalize_provider_function_schema(mut schema: Value) -> Value {
    normalize_provider_function_schema_in_place(&mut schema);
    schema
}

pub fn normalize_openai_json_schema_in_place(schema: &mut Value) {
    match schema {
        Value::Object(object) => normalize_schema_object(object),
        Value::Array(items) => {
            for item in items {
                normalize_openai_json_schema_in_place(item);
            }
        }
        _ => {}
    }
}

pub fn normalize_provider_function_schema_in_place(schema: &mut Value) {
    match schema {
        Value::Object(object) => normalize_provider_schema_object(object),
        Value::Array(items) => {
            for item in items {
                normalize_provider_function_schema_in_place(item);
            }
        }
        _ => {}
    }
}

fn normalize_schema_object(object: &mut Map<String, Value>) {
    let is_object_schema = object.get("type").and_then(Value::as_str) == Some("object")
        || object.contains_key("properties");
    if is_object_schema {
        object
            .entry("properties".to_string())
            .or_insert_with(|| json!({}));
        object
            .entry("additionalProperties".to_string())
            .or_insert_with(|| json!(false));
    }

    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for property_schema in properties.values_mut() {
            normalize_openai_json_schema_in_place(property_schema);
        }
    }

    if let Some(items) = object.get_mut("items") {
        normalize_openai_json_schema_in_place(items);
    }

    if let Some(additional_properties) = object.get_mut("additionalProperties")
        && !additional_properties.is_boolean()
    {
        normalize_openai_json_schema_in_place(additional_properties);
    }

    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(values) = object.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                normalize_openai_json_schema_in_place(value);
            }
        }
    }

    for key in ["$defs", "definitions"] {
        if let Some(definitions) = object.get_mut(key).and_then(Value::as_object_mut) {
            for definition in definitions.values_mut() {
                normalize_openai_json_schema_in_place(definition);
            }
        }
    }
}

fn normalize_provider_schema_object(object: &mut Map<String, Value>) {
    normalize_schema_object(object);

    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required = properties
            .keys()
            .cloned()
            .map(Value::String)
            .collect::<Vec<_>>();
        object.insert("required".to_string(), Value::Array(required));
    }

    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for property_schema in properties.values_mut() {
            normalize_provider_function_schema_in_place(property_schema);
        }
    }

    if let Some(items) = object.get_mut("items") {
        normalize_provider_function_schema_in_place(items);
    }

    if let Some(additional_properties) = object.get_mut("additionalProperties")
        && !additional_properties.is_boolean()
    {
        normalize_provider_function_schema_in_place(additional_properties);
    }

    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(values) = object.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                normalize_provider_function_schema_in_place(value);
            }
        }
    }

    for key in ["$defs", "definitions"] {
        if let Some(definitions) = object.get_mut(key).and_then(Value::as_object_mut) {
            for definition in definitions.values_mut() {
                normalize_provider_function_schema_in_place(definition);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_openai_json_schema, normalize_provider_function_schema};
    use serde_json::json;

    fn sorted_required_list(value: &serde_json::Value) -> Vec<String> {
        let mut fields = value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        fields.sort();
        fields
    }

    #[test]
    fn adds_additional_properties_false_recursively_for_object_schemas() {
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
                "details": {
                    "type": "object",
                    "properties": {
                        "count": { "type": "integer" }
                    }
                },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" }
                        }
                    }
                }
            }
        });

        let normalized = normalize_openai_json_schema(schema);

        assert_eq!(normalized["additionalProperties"], json!(false));
        assert_eq!(
            normalized["properties"]["details"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            normalized["properties"]["items"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn preserves_existing_additional_properties_values() {
        let schema = json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "config": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "string"
                    }
                }
            }
        });

        let normalized = normalize_openai_json_schema(schema);

        assert_eq!(normalized["additionalProperties"], json!(true));
        assert_eq!(
            normalized["properties"]["config"]["additionalProperties"]["type"],
            json!("string")
        );
    }

    #[test]
    fn provider_schema_marks_all_object_properties_as_required_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "rationale": { "type": "string" },
                "test_demo_groups": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "test": { "type": "string" },
                            "demos": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "title": { "type": "string" },
                                        "must_use_tools": { "type": "boolean" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let normalized = normalize_provider_function_schema(schema);

        assert_eq!(
            sorted_required_list(&normalized["required"]),
            vec!["rationale".to_string(), "test_demo_groups".to_string()]
        );
        assert_eq!(
            sorted_required_list(
                &normalized["properties"]["test_demo_groups"]["items"]["required"]
            ),
            vec!["demos".to_string(), "test".to_string()]
        );
        assert_eq!(
            sorted_required_list(
                &normalized["properties"]["test_demo_groups"]["items"]["properties"]["demos"]["items"]
                    ["required"]
            ),
            vec!["must_use_tools".to_string(), "title".to_string()]
        );
    }
}
