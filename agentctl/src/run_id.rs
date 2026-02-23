use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

pub fn new_run_id() -> String {
    format!("{}", Uuid::new_v4())
}

pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
