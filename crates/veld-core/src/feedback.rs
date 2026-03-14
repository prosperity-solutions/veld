use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackComment {
    #[serde(default)]
    pub id: String,
    pub page_url: String,
    pub element_selector: Option<String>,
    pub selected_text: Option<String>,
    pub comment: String,
    pub position: Option<ElementPosition>,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementPosition {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackBatch {
    pub id: String,
    pub run_name: String,
    pub comments: Vec<FeedbackComment>,
    pub submitted_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// File-based feedback store. Layout:
///   .veld/feedback/{run_name}/drafts.json   — current draft comments
///   .veld/feedback/{run_name}/batches/      — submitted batches
pub struct FeedbackStore {
    drafts_path: PathBuf,
    batches_dir: PathBuf,
    run_name: String,
}

impl FeedbackStore {
    pub fn new(project_root: &Path, run_name: &str) -> Self {
        let base = project_root.join(".veld").join("feedback").join(run_name);
        Self {
            drafts_path: base.join("drafts.json"),
            batches_dir: base.join("batches"),
            run_name: run_name.to_owned(),
        }
    }

    /// Check whether any feedback data (drafts or batches) exists for this run.
    pub fn has_data(&self) -> bool {
        self.drafts_path.exists() || self.batches_dir.exists()
    }

    fn ensure_dirs(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.drafts_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&self.batches_dir)?;
        Ok(())
    }

    // -- Drafts ---------------------------------------------------------------

    pub fn get_comments(&self) -> anyhow::Result<Vec<FeedbackComment>> {
        if !self.drafts_path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(&self.drafts_path)?;
        let comments: Vec<FeedbackComment> = serde_json::from_str(&data)?;
        Ok(comments)
    }

    pub fn save_comment(&self, comment: &FeedbackComment) -> anyhow::Result<()> {
        self.ensure_dirs()?;
        let mut comments = self.get_comments()?;
        comments.push(comment.clone());
        std::fs::write(&self.drafts_path, serde_json::to_string_pretty(&comments)?)?;
        Ok(())
    }

    pub fn update_comment(&self, updated: &FeedbackComment) -> anyhow::Result<bool> {
        let mut comments = self.get_comments()?;
        if let Some(existing) = comments.iter_mut().find(|c| c.id == updated.id) {
            *existing = updated.clone();
            std::fs::write(&self.drafts_path, serde_json::to_string_pretty(&comments)?)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn delete_comment(&self, id: &str) -> anyhow::Result<bool> {
        let mut comments = self.get_comments()?;
        let before = comments.len();
        comments.retain(|c| c.id != id);
        if comments.len() == before {
            return Ok(false);
        }
        std::fs::write(&self.drafts_path, serde_json::to_string_pretty(&comments)?)?;
        Ok(true)
    }

    // -- Batches --------------------------------------------------------------

    pub fn submit_batch(&self) -> anyhow::Result<FeedbackBatch> {
        self.ensure_dirs()?;
        let comments = self.get_comments()?;
        let batch = FeedbackBatch {
            id: Uuid::new_v4().to_string(),
            run_name: self.run_name.clone(),
            comments,
            submitted_at: Utc::now(),
        };
        let batch_path = self.batches_dir.join(format!("{}.json", batch.id));
        std::fs::write(&batch_path, serde_json::to_string_pretty(&batch)?)?;
        // Clear drafts after submit.
        std::fs::write(&self.drafts_path, "[]")?;
        Ok(batch)
    }

    pub fn get_batches(&self) -> anyhow::Result<Vec<FeedbackBatch>> {
        if !self.batches_dir.exists() {
            return Ok(Vec::new());
        }
        let mut batches = Vec::new();
        for entry in std::fs::read_dir(&self.batches_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                let data = std::fs::read_to_string(&path)?;
                if let Ok(batch) = serde_json::from_str::<FeedbackBatch>(&data) {
                    batches.push(batch);
                }
            }
        }
        batches.sort_by(|a, b| a.submitted_at.cmp(&b.submitted_at));
        Ok(batches)
    }

    pub fn get_latest_batch(&self) -> anyhow::Result<Option<FeedbackBatch>> {
        let batches = self.get_batches()?;
        Ok(batches.into_iter().last())
    }

    pub fn get_batches_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<FeedbackBatch>> {
        let batches = self.get_batches()?;
        Ok(batches
            .into_iter()
            .filter(|b| b.submitted_at > since)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_comment(text: &str) -> FeedbackComment {
        FeedbackComment {
            id: Uuid::new_v4().to_string(),
            page_url: "https://example.com".into(),
            element_selector: Some("div.main".into()),
            selected_text: None,
            comment: text.into(),
            position: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_crud_comments() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path(), "test-run");

        assert!(store.get_comments().unwrap().is_empty());

        let c1 = make_comment("first");
        store.save_comment(&c1).unwrap();
        assert_eq!(store.get_comments().unwrap().len(), 1);

        let c2 = make_comment("second");
        store.save_comment(&c2).unwrap();
        assert_eq!(store.get_comments().unwrap().len(), 2);

        let mut updated = c1.clone();
        updated.comment = "updated first".into();
        assert!(store.update_comment(&updated).unwrap());

        let comments = store.get_comments().unwrap();
        assert_eq!(comments[0].comment, "updated first");

        assert!(store.delete_comment(&c2.id).unwrap());
        assert_eq!(store.get_comments().unwrap().len(), 1);

        assert!(!store.delete_comment("nonexistent").unwrap());
    }

    #[test]
    fn test_submit_batch() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path(), "test-run");

        store.save_comment(&make_comment("a")).unwrap();
        store.save_comment(&make_comment("b")).unwrap();

        let batch = store.submit_batch().unwrap();
        assert_eq!(batch.comments.len(), 2);
        assert_eq!(batch.run_name, "test-run");

        // Drafts should be cleared after submit.
        assert!(store.get_comments().unwrap().is_empty());

        // Batch should be retrievable.
        let batches = store.get_batches().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].id, batch.id);
    }
}
