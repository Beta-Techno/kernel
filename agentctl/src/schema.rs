use anyhow::{Context, Result, anyhow, bail};
use jsonschema::JSONSchema;
use serde_json::Value;

const WORK_UNIT_SCHEMA: &str = include_str!("../../runfmt/work_unit.schema.json");
const RUN_RECORD_SCHEMA: &str = include_str!("../../runfmt/run_record.schema.json");

pub fn validate_work_unit(instance: &Value) -> Result<()> {
    validate_with_schema(instance, WORK_UNIT_SCHEMA, "runfmt/work_unit.schema.json")
}

pub fn validate_run_record(instance: &Value) -> Result<()> {
    validate_with_schema(instance, RUN_RECORD_SCHEMA, "runfmt/run_record.schema.json")
}

fn validate_with_schema(instance: &Value, raw_schema: &str, schema_name: &str) -> Result<()> {
    let schema_json: Value = serde_json::from_str(raw_schema)
        .with_context(|| format!("failed to parse JSON schema {schema_name}"))?;
    let compiled = JSONSchema::compile(&schema_json)
        .map_err(|err| anyhow!("failed to compile JSON schema {schema_name}: {err}"))?;

    if let Err(errors) = compiled.validate(instance) {
        let details = errors
            .map(|err| format!("{}: {}", err.instance_path, err))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "schema validation failed against {}:\n{}",
            schema_name,
            details
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_example_work_unit() {
        let sample: Value =
            serde_json::from_str(include_str!("../runfmt-example.json")).expect("valid sample");
        validate_work_unit(&sample).expect("sample should match schema");
    }

    #[test]
    fn rejects_invalid_workspace_mode() {
        let mut sample: Value =
            serde_json::from_str(include_str!("../runfmt-example.json")).expect("valid sample");
        sample["target"]["workspace_mode"] = Value::String("invalid".to_string());
        let err = validate_work_unit(&sample).expect_err("validation must fail");
        assert!(err.to_string().contains("schema validation failed"));
    }

    #[test]
    fn rejects_invalid_work_unit_ids() {
        let invalid_ids = ["", "abc:def", ".", ".."];
        for id in invalid_ids {
            let mut sample: Value =
                serde_json::from_str(include_str!("../runfmt-example.json")).expect("valid sample");
            sample["id"] = Value::String(id.to_string());
            let err = validate_work_unit(&sample).expect_err("validation must fail");
            assert!(
                err.to_string().contains("schema validation failed"),
                "unexpected error for id={id:?}: {err}"
            );
        }
    }
}
