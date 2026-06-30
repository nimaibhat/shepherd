//! Headless agent runners and workspace seeding.

pub mod claude;
pub mod seed;

pub use claude::{ClaudeRunner, ClaudeStreamParser};
pub use seed::seed_workspace;
