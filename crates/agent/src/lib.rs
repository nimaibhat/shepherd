//! Headless agent runners and workspace seeding.

pub mod capture;
pub mod claude;
pub mod seed;

pub use capture::capture_local_workspace;
pub use claude::{ClaudeRunner, ClaudeStreamParser};
pub use seed::seed_workspace;
