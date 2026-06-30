//! Concrete SandboxProvider adapters.

pub mod daytona;
pub mod docker;

pub use daytona::DaytonaProvider;
pub use docker::DockerProvider;
