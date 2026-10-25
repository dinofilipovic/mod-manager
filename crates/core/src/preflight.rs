//! Checks before the game starts: what would keep mods from working. Built
//! by `Library::preflight` from the loadout, the game folder and the install.

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum PreflightSeverity {
    /// Mods will fail: launching asks first.
    Error,
    /// Worth knowing; doesn't stop the launch.
    Warning,
}

/// What a preflight item asks the user to do.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "action")]
#[ts(export)]
pub enum PreflightAction {
    Apply,
    FixDllOverrides,
    EnableMod { mod_id: String },
    OpenNexus { nexus_mod_id: u32 },
    ChooseOptions { token: String },
    ShowMod { mod_id: String },
    None,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct PreflightItem {
    pub severity: PreflightSeverity,
    pub title: String,
    pub detail: String,
    pub action: PreflightAction,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct Preflight {
    pub items: Vec<PreflightItem>,
}

impl Preflight {
    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|i| i.severity == PreflightSeverity::Error)
    }
}
