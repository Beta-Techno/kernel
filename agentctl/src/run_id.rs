use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

pub fn new_run_id() -> String {
    format!("{}", Uuid::new_v4())
}

pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn validate_user_supplied(run_id: &str) -> Result<()> {
    if run_id.is_empty() {
        bail!("run id must not be empty");
    }
    if run_id.len() > 128 {
        bail!("run id must be at most 128 characters");
    }
    if run_id == "." || run_id == ".." {
        bail!("run id must not be . or ..");
    }
    if !run_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        bail!("run id contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_safe_ids() {
        assert!(validate_user_supplied("run-123").is_ok());
        assert!(validate_user_supplied("A.B_c-9").is_ok());
    }

    #[test]
    fn rejects_unsafe_ids() {
        assert!(validate_user_supplied("").is_err());
        assert!(validate_user_supplied(":bad").is_err());
        assert!(validate_user_supplied(".").is_err());
        assert!(validate_user_supplied("..").is_err());
    }
}
