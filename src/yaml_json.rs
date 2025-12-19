use anyhow::Context;

/// Convert YAML into a JSON value suitable for jsonschema validation.
///
/// Notes:
/// - YAML allows non-string mapping keys; JSON does not. We error on such keys.
/// - YAML numbers are mapped to i64/u64/f64 when representable.
pub fn yaml_to_json_value(v: &serde_yaml::Value) -> anyhow::Result<serde_json::Value> {
    Ok(match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq {
                out.push(yaml_to_json_value(item)?);
            }
            serde_json::Value::Array(out)
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s.clone(),
                    other => {
                        anyhow::bail!(
                            "YAML mapping key must be string (got {other:?})"
                        );
                    }
                };
                obj.insert(key, yaml_to_json_value(val)?);
            }
            serde_json::Value::Object(obj)
        }
        // If serde_yaml adds more variants in the future, serialize fallback.
        other => serde_json::to_value(other)
            .context("serialize YAML value to JSON")?,
    })
}
