// Library root: exposes modules that integration tests (tests/) need to import.
// The binary (src/main.rs) declares its own `mod config` independently; both
// compile from the same source so behaviour is identical.

pub mod config;
pub mod history;
