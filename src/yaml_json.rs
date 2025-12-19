use anyhow::Context;
use serde_json::{Map, Number, Value as JsonValue};

/// Convert `serde_yaml::Value` to `serde_json::Value`.
pub fn yaml_to_json_value(v: &serde_yaml::Value) -> anyhow::Result<JsonValue> {
    match v {
        serde_yaml::Value::Null => Ok(JsonValue::Null),
        serde_yaml::Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(JsonValue::Number(Number::from(i)))
            } else if let Some(u) = n.as_u64() {
                Ok(JsonValue::Number(Number::from(u)))
            } else if let Some(f) = n.as_f64() {
                let num = Number::from_f64(f).context("YAML float is not representable as JSON")?;
                Ok(JsonValue::Number(num))
            } else {
                Ok(JsonValue::Null)
            }
        }
        serde_yaml::Value::String(s) => Ok(JsonValue::String(s.clone())),
        serde_yaml::Value::Sequence(seq) => {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq {
                out.push(yaml_to_json_value(item)?);
            }
            Ok(JsonValue::Array(out))
        }
        serde_yaml::Value::Mapping(map) => {
            let mut out = Map::new();
            for (k, val) in map.iter() {
                let key = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => {
                        // JSON object keys must be strings. Best-effort stringify.
                        format!("{other:?}")
                    }
                };
                out.insert(key, yaml_to_json_value(val)?);
            }
            Ok(JsonValue::Object(out))
        }
        // Tagged values are rare; stringify the tag and continue.
        serde_yaml::Value::Tagged(tagged) => yaml_to_json_value(&tagged.value),
    }
}
