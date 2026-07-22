//! Instance-wide settings (maintenance mode, banner).
//!
//! Uses a process-global `RwLock` so the admin API can toggle settings
//! without requiring AppState mutations or server restarts.

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstanceSettings {
    /// When true, only GET/HEAD/OPTIONS and admin routes are served.
    pub maintenance_mode: bool,
    /// Optional banner message shown to all users.
    pub banner_message: Option<String>,
    /// Banner type: "info", "warning", "error"
    pub banner_type: String,
}

impl InstanceSettings {
    pub fn is_banner_active(&self) -> bool {
        self.banner_message.is_some()
    }
}

static SETTINGS: RwLock<InstanceSettings> = RwLock::new(InstanceSettings {
    maintenance_mode: false,
    banner_message: None,
    banner_type: String::new(),
});

/// Read the current instance settings.
pub fn get_settings() -> InstanceSettings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Update instance settings (e.g. from admin API).
pub fn update_settings(f: impl FnOnce(&mut InstanceSettings)) {
    let mut guard = SETTINGS.write().unwrap_or_else(|e| e.into_inner());
    f(&mut guard);
}
