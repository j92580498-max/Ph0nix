//! Reusable CPU, memory and bus mechanisms.
pub mod bus;
pub mod cpu;
pub mod memory;
pub mod report;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Unicorn {operation} failed: {error}")]
    Unicorn {
        operation: &'static str,
        error: String,
    },
}
