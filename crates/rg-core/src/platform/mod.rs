//! Platform-specific abstractions for cross-platform compatibility.
//!
//! This module provides unified interfaces for operations that differ
//! between Unix and Windows (paths, process management, file permissions, etc.).

pub mod fs;
pub mod path;
pub mod process;

// Re-export commonly used items
pub use fs::{is_executable, set_executable};
pub use path::{expand_home, repo_path, temp_dir, validate_repo_path};
pub use process::execute_script;
