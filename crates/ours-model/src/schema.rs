//! JSON Schemas for the small-model calls, shaped for strict servers.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;

/// The schema of `T`, inlined, without the keywords strict servers reject,
/// and with every object closed and all its properties required.
pub(crate) fn strict_schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::default()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator();
    let mut schema = Value::from(generator.into_root_schema_for::<T>());
    strip_keys(&mut schema, &["$schema", "title", "format"]);
    close_objects(&mut schema);
    schema
}

/// Removes keywords that strict structured-output servers reject.
fn strip_keys(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in keys {
                // A property may itself be called "title" or "format".
                if map.get(*key).is_some_and(|v| !v.is_object()) {
                    map.remove(*key);
                }
            }
            map.values_mut().for_each(|v| strip_keys(v, keys));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| strip_keys(v, keys)),
        _ => {}
    }
}

/// Sets `additionalProperties: false` on every object schema and lists all
/// of its properties as required. Optional fields keep a nullable type.
fn close_objects(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("properties") {
                let names: Vec<Value> = props.keys().cloned().map(Value::String).collect();
                map.insert("required".to_owned(), Value::Array(names));
                map.insert("additionalProperties".to_owned(), Value::Bool(false));
            }
            for (key, child) in map.iter_mut() {
                match child {
                    // The values here are schemas; the keys are field names.
                    Value::Object(props) if key == "properties" => {
                        props.values_mut().for_each(close_objects);
                    }
                    _ => close_objects(child),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(close_objects),
        _ => {}
    }
}
