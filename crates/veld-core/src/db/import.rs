//! One-time, best-effort import of pre-SQLite state files.
//!
//! Ensures environments started by an older veld remain visible and
//! stoppable after upgrading: the global registry, each project's
//! `.veld/state.json` runs, the relay-token cache, and the hint counter are
//! copied into the database on the first open of the default database.
//!
//! The import is deliberately forgiving: corrupt or missing files are
//! skipped, the old files are left untouched (they are simply never read
//! again), and the whole pass runs at most once (guarded by a kv flag).
//! Logs and feedback threads are not imported — they are ephemeral and
//! age-pruned anyway.

use std::collections::HashMap;

use crate::state::{GlobalRegistry, RunState};

use super::Db;

const IMPORT_FLAG: &str = "legacy.imported_at";

impl Db {
    /// Run the legacy import once. Never fails — logs and moves on.
    pub(super) fn import_legacy_files_once(&self) {
        match self.kv_get(IMPORT_FLAG) {
            Ok(None) => {}
            _ => return, // already imported (or kv unreadable — don't loop)
        }

        self.import_registry_and_runs();
        self.import_relay_tokens();
        self.import_hints();

        let _ = self.kv_set(IMPORT_FLAG, &super::now_str());
    }

    fn import_registry_and_runs(&self) {
        let Some(registry_path) = dirs::data_dir().map(|d| d.join("veld").join("registry.json"))
        else {
            return;
        };
        let Ok(data) = std::fs::read_to_string(&registry_path) else {
            return; // nothing to import
        };
        let Ok(registry) = serde_json::from_str::<GlobalRegistry>(&data) else {
            tracing::warn!("legacy registry.json is unreadable — skipping import");
            return;
        };

        let mut imported = 0usize;
        for entry in registry.projects.values() {
            let state_path = entry.project_root.join(".veld").join("state.json");
            let Ok(state_data) = std::fs::read_to_string(&state_path) else {
                continue;
            };
            // Parse without decrypting: values encrypted at rest stay
            // encrypted, and `save_run` only encrypts what isn't already.
            #[derive(serde::Deserialize)]
            struct LegacyProjectState {
                #[serde(default)]
                runs: HashMap<String, RunState>,
            }
            let Ok(state) = serde_json::from_str::<LegacyProjectState>(&state_data) else {
                tracing::warn!(
                    path = %state_path.display(),
                    "legacy state.json is unreadable — skipping project import"
                );
                continue;
            };
            for run in state.runs.values() {
                if self
                    .save_run(&entry.project_root, &entry.project_name, run)
                    .is_ok()
                {
                    imported += 1;
                }
            }
        }
        if imported > 0 {
            tracing::info!(
                runs = imported,
                "imported legacy run state into the veld database"
            );
        }
    }

    fn import_relay_tokens(&self) {
        let Some(path) = dirs::data_dir().map(|d| d.join("veld").join("relay-tokens.json")) else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(map) = serde_json::from_slice::<std::collections::BTreeMap<String, String>>(&bytes)
        else {
            return;
        };
        for (url, token) in &map {
            let _ = self.save_relay_token(url, token);
        }
    }

    fn import_hints(&self) {
        let Some(path) = dirs::home_dir().map(|h| h.join(".veld").join("hints.json")) else {
            return;
        };
        let Ok(data) = std::fs::read_to_string(&path) else {
            return;
        };
        let count = serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|v| v.get("privileged_hint_count").and_then(|c| c.as_u64()));
        if let Some(count) = count {
            let _ = self.kv_set("hints.privileged_hint_count", &count.to_string());
        }
    }
}
