//! Mirror service — sync and manage repository mirrors from remote sources.
/// LFS objects a mirror pass fetches after the refs.
pub(crate) mod lfs;
/// The background scheduler that runs the periodic half of the feature.
pub mod scheduler;
/// Mirror service for repository mirroring.
pub mod service;
/// Instance-level confidentiality policy for outbound mirror transport.
pub mod transport;
/// The refusal that keeps a pull mirror's branches and tags its upstream's.
pub mod write_guard;
