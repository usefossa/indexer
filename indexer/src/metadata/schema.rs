use serde_json::Value;
use std::{collections::HashSet, sync::LazyLock};

pub const MAX_BYTES: usize = 32_768;
pub const MAX_DEPTH: usize = 64;
pub const SCHEMA: &str = include_str!("schema.json");

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Provider,
    Model,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Model => "model",
        }
    }
}

static VALIDATORS: LazyLock<[jsonschema::Validator; 2]> = LazyLock::new(|| {
    [Kind::Provider, Kind::Model].map(|kind| {
        let mut schema: Value =
            serde_json::from_str(SCHEMA).expect("valid bundled metadata schema");
        schema.as_object_mut().unwrap().remove("anyOf");
        schema["$ref"] = format!("#/$defs/{}", kind.name()).into();
        jsonschema::draft202012::options()
            .should_validate_formats(true)
            .build(&schema)
            .expect("valid bundled metadata schema")
    })
});

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| value.as_f64().unwrap() as u64)
}

pub fn validate(kind: Kind, value: &Value) -> Result<(), String> {
    let mut pending = vec![(value, 1)];
    while let Some((value, depth)) = pending.pop() {
        if (value.is_object() || value.is_array()) && depth > MAX_DEPTH {
            return Err(format!("Metadata exceeds {MAX_DEPTH} container levels"));
        }
        match value {
            Value::Object(fields) => {
                pending.extend(fields.values().map(|child| (child, depth + 1)))
            }
            Value::Array(items) => pending.extend(items.iter().map(|child| (child, depth + 1))),
            _ => (),
        }
    }
    let validator = &VALIDATORS[match kind {
        Kind::Provider => 0,
        Kind::Model => 1,
    }];
    if let Err(error) = validator.validate(value) {
        return Err(format!(
            "Invalid {} metadata at {}: {}",
            kind.name(),
            error.instance_path(),
            error.schema_path()
        ));
    }
    if matches!(kind, Kind::Model) {
        if value
            .get("inputBoundTokens")
            .is_some_and(|bound| number(bound) > number(&value["contextTokens"]))
        {
            return Err("Invalid model metadata: inputBoundTokens exceeds contextTokens".into());
        }
        if value
            .get("settings")
            .is_some_and(|settings| number(&settings["maxTokens"]) > number(&value["outputTokens"]))
        {
            return Err("Invalid model metadata: settings.maxTokens exceeds outputTokens".into());
        }
        let mut ids = HashSet::new();
        for variant in value
            .get("variants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !ids.insert(variant["id"].as_str().unwrap()) {
                return Err("Invalid model metadata: duplicate variant id".into());
            }
        }
        if let Some(variants) = value.get("variants").and_then(Value::as_array) {
            let effort = value["effort"].as_array().unwrap();
            if variants.len() != effort.len()
                || variants
                    .iter()
                    .zip(effort)
                    .any(|(variant, label)| variant["id"] != *label)
            {
                return Err(
                    "Invalid model metadata: variant ids must match effort in order".into(),
                );
            }
        }
    }
    Ok(())
}

pub fn parse(kind: Kind, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("Metadata exceeds {MAX_BYTES} bytes"));
    }
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Metadata must be valid UTF-8 JSON")?;
    validate(kind, &value)?;
    // json schema integers include 1.0; normalize parsed counters without changing committed bytes.
    if matches!(kind, Kind::Model) {
        for field in ["contextTokens", "outputTokens", "inputBoundTokens"] {
            if let Some(value) = value.get_mut(field) {
                *value = number(value).into();
            }
        }
        if let Some(value) = value
            .get_mut("settings")
            .and_then(|settings| settings.get_mut("maxTokens"))
        {
            *value = number(value).into();
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_conformance_cases() {
        let cases: Value = serde_json::from_str(include_str!("fixtures.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let kind = if case["kind"] == "provider" {
                Kind::Provider
            } else {
                Kind::Model
            };
            let bytes = case
                .get("raw")
                .and_then(Value::as_str)
                .map(|raw| raw.as_bytes().to_vec())
                .unwrap_or_else(|| serde_json::to_vec(&case["document"]).unwrap());
            let result = parse(kind, &bytes);
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}: {result:?}",
                case["name"]
            );
        }
    }

    #[test]
    fn rejects_oversized_or_invalid_utf8_and_normalizes_integer_notation() {
        assert!(parse(Kind::Provider, &vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(parse(Kind::Provider, b"{\"name\":\"\xff\"}").is_err());
        let bytes = br#"{"id":"example/model","name":"Model","modalities":{"input":["text"],"output":["text"]},"contextTokens":100.0,"outputTokens":10e0}"#;
        let value = parse(Kind::Model, bytes).unwrap();
        assert_eq!(value["contextTokens"].as_u64(), Some(100));
        assert_eq!(value["outputTokens"].as_u64(), Some(10));
    }
}
