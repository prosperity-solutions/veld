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
use std::path::Path;

use crate::state::{GlobalRegistry, RunState};

use super::Db;

const IMPORT_FLAG: &str = "legacy.imported_at";

impl Db {
    /// Run the legacy import once. Never fails — logs and moves on.
    pub(super) fn import_legacy_files_once(&self) {
        let legacy_data_dir = dirs::data_dir().map(|d| d.join("veld"));
        let legacy_home_dir = dirs::home_dir().map(|h| h.join(".veld"));
        self.import_legacy_from(legacy_data_dir.as_deref(), legacy_home_dir.as_deref());
    }

    /// The import body, with explicit source directories so it is testable.
    /// `data_dir` held `registry.json` + `relay-tokens.json`; `home_dir`
    /// held `hints.json`.
    ///
    /// The flag is deliberately set only AFTER a completed pass, not claimed
    /// up front: every step is an idempotent upsert, so the worst outcome of
    /// two processes racing right after an upgrade is a harmless double
    /// import — whereas claim-first would let a crash mid-import abandon it
    /// half-done forever, stranding pre-upgrade environments (the exact
    /// invariant this import protects).
    fn import_legacy_from(&self, data_dir: Option<&Path>, home_dir: Option<&Path>) {
        match self.kv_get(IMPORT_FLAG) {
            Ok(None) => {}
            _ => return, // already imported (or kv unreadable — don't loop)
        }

        if let Some(data_dir) = data_dir {
            self.import_registry_and_runs(&data_dir.join("registry.json"));
            self.import_relay_tokens(&data_dir.join("relay-tokens.json"));
        }
        if let Some(home_dir) = home_dir {
            self.import_hints(&home_dir.join("hints.json"));
        }

        let _ = self.kv_set(IMPORT_FLAG, &super::now_str());
    }

    fn import_registry_and_runs(&self, registry_path: &Path) {
        let Ok(data) = std::fs::read_to_string(registry_path) else {
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

    fn import_relay_tokens(&self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
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

    fn import_hints(&self, path: &Path) {
        let Ok(data) = std::fs::read_to_string(path) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::state::{NodeState, RegistryEntry, RunStatus};

    /// Seed a legacy layout: registry.json + per-project state.json +
    /// relay-tokens.json in `data`, hints.json in `home`.
    fn seed_legacy(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let data = dir.join("data");
        let home = dir.join("home");
        let project = dir.join("projA");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".veld")).unwrap();

        let mut run = RunState::new("dev", "proj");
        run.status = RunStatus::Running;
        let mut node = NodeState::new("web", "local");
        node.pid = Some(1234);
        node.outputs
            .insert("token".into(), crate::sensitive::encrypt_value("s3cret"));
        node.sensitive_keys = vec!["token".into()];
        run.nodes.insert("web:local".into(), node);

        let state = serde_json::json!({ "runs": { "dev": run } });
        std::fs::write(
            project.join(".veld").join("state.json"),
            serde_json::to_string(&state).unwrap(),
        )
        .unwrap();

        let mut registry = GlobalRegistry::default();
        registry.projects.insert(
            project.to_string_lossy().into_owned(),
            RegistryEntry {
                project_root: project.clone(),
                project_name: "proj".into(),
                runs: HashMap::new(),
            },
        );
        std::fs::write(
            data.join("registry.json"),
            serde_json::to_string(&registry).unwrap(),
        )
        .unwrap();

        std::fs::write(
            data.join("relay-tokens.json"),
            r#"{"https://relay.example/":"tok-1"}"#,
        )
        .unwrap();
        std::fs::write(home.join("hints.json"), r#"{"privileged_hint_count":3}"#).unwrap();

        (data, home, project)
    }

    #[test]
    fn imports_runs_tokens_and_hints_once() {
        let (dir, db) = test_db();
        let (data, home, project) = seed_legacy(dir.path());

        db.import_legacy_from(Some(&data), Some(&home));

        // Run state landed, sensitive value decrypts on load.
        let run = db.get_run(&project, "dev").unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Running);
        let node = &run.nodes["web:local"];
        assert_eq!(node.pid, Some(1234));
        assert_eq!(node.outputs["token"], "s3cret");

        // Tokens + hints landed.
        assert_eq!(db.relay_tokens()["https://relay.example/"], "tok-1");
        assert_eq!(
            db.kv_get("hints.privileged_hint_count").unwrap().as_deref(),
            Some("3")
        );
        assert!(db.kv_get(IMPORT_FLAG).unwrap().is_some());

        // Second call is a no-op: mutate the DB, re-import, nothing reverts.
        db.remove_run(&project, "dev").unwrap();
        db.import_legacy_from(Some(&data), Some(&home));
        assert!(db.get_run(&project, "dev").unwrap().is_none());
    }

    #[test]
    fn corrupt_or_missing_files_are_skipped() {
        let (dir, db) = test_db();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("registry.json"), "{ not json").unwrap();
        std::fs::write(data.join("relay-tokens.json"), "also not json").unwrap();

        // Must not panic; flag still set so it never loops.
        db.import_legacy_from(Some(&data), None);
        assert!(db.registry().unwrap().projects.is_empty());
        assert!(db.relay_tokens().is_empty());
        assert!(db.kv_get(IMPORT_FLAG).unwrap().is_some());
    }
}
