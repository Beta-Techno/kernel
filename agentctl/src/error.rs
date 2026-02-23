use thiserror::Error;

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("invalid runfmt version: {0}")]
    InvalidVersion(String),
}
