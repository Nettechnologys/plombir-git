//! Mirror service — sync and manage repository mirrors from remote sources.
/// The background scheduler that runs the periodic half of the feature.
pub mod scheduler;
/// Mirror service for repository mirroring.
pub mod service;
