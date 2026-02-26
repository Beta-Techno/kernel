use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

const MAX_RUN_ID_LEN: usize = 128;

pub fn new_run_id() -> String {
    format!("{}", Uuid::new_v4())
}

pub fn validate_user_run_id(run_id: &str) -> Result<()> {
    if run_id.is_empty() {
        bail!("run id must not be empty");
    }
    if run_id.len() > MAX_RUN_ID_LEN {
        bail!("run id too long (max {MAX_RUN_ID_LEN})");
    }
    if run_id == "." || run_id == ".." {
        bail!("run id must not be '.' or '..'");
    }
    if !run_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        bail!("run id contains unsupported characters");
    }
    Ok(())
}

pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_safe_ids() {
        validate_user_run_id("conductor-smoke-001").expect("id should be valid");
        validate_user_run_id("01J0EXAMPLE.test").expect("id should be valid");
    }

    #[test]
    fn rejects_unsafe_ids() {
        for value in ["", ".", "..", "a:b", "a/b", "a\\b"] {
            let err = validate_user_run_id(value).expect_err("id should be invalid");
            assert!(!err.to_string().is_empty());
        }
    }
}
