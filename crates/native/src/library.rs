//! A project chat catalog. Each conversation has its own bounded archive.
use crate::{
    Wake,
    history::Store,
    journal::{digest, lock, read_checked, write_checked},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub fn new_id() -> String {
    digest(
        format!(
            "{:?}:{}:{}",
            std::time::SystemTime::now(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    )
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub title: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    active: String,
    entries: Vec<Entry>,
}
pub struct Library {
    root: PathBuf,
    index: Index,
    fallback: Option<PathBuf>,
    _lease: File,
}
impl Library {
    pub fn open(profile: &Path, host: &str, document: &str) -> Result<Self, String> {
        if host.len() > 128 || document.len() > 32768 {
            return Err("Invalid chat catalog identity".into());
        }
        let parent = profile.parent().ok_or("Cannot locate chat catalog")?;
        let name = profile
            .file_name()
            .ok_or("Invalid preferences profile")?
            .to_string_lossy();
        let root = parent
            .join("ai-chat-history")
            .join("conversations")
            .join(digest(
                serde_json::to_string(&(name.as_ref(), host, document))
                    .map_err(|_| "Cannot encode chat identity")?
                    .as_bytes(),
            ));
        crate::journal::private_dir(&root)?;
        let lease = lock(&root.join("writer.lock"))?;
        let loaded = read_checked(&root.join("index.json"), 128 * 1024)?;
        let fresh = loaded.is_none();
        let index = if let Some(value) = loaded {
            serde_json::from_value::<Index>(value)
                .map_err(|_| "Invalid chat catalog; original files were preserved")?
        } else {
            let id = new_id();
            Index {
                version: 1,
                active: id.clone(),
                entries: vec![Entry {
                    id,
                    title: "Previous chat".into(),
                }],
            }
        };
        let mut ids = std::collections::HashSet::new();
        if index.version != 1
            || index.entries.is_empty()
            || index.entries.len() > 512
            || index
                .entries
                .iter()
                .any(|e| !valid_id(&e.id) || e.title.len() > 256 || !ids.insert(e.id.clone()))
            || !ids.contains(&index.active)
        {
            return Err("Invalid chat catalog; original files were preserved".into());
        }
        let fallback = (fresh
            || (index
                .entries
                .first()
                .is_some_and(|entry| entry.id == index.active && entry.title == "Previous chat")))
        .then(|| {
            parent.join("ai-chat-history").join(format!(
                "{:x}.json",
                Sha256::digest(format!("{name}\n{host}\n{document}").as_bytes())
            ))
        });
        let library = Self {
            root,
            index,
            fallback,
            _lease: lease,
        };
        library.save()?;
        Ok(library)
    }
    fn save(&self) -> Result<(), String> {
        write_checked(
            &self.root.join("index.json"),
            &serde_json::to_value(&self.index).map_err(|_| "Cannot encode chat catalog")?,
            128 * 1024,
            true,
        )
    }
    pub fn entries(&self) -> &[Entry] {
        &self.index.entries
    }
    pub fn active(&self) -> &str {
        &self.index.active
    }
    pub fn store(&mut self, wake: Wake) -> Result<Store, String> {
        Store::open_path(
            self.root.join(format!("{}.json", self.index.active)),
            self.fallback.take(),
            wake,
        )
    }
    pub fn select(&mut self, id: &str) -> Result<(), String> {
        if !self.index.entries.iter().any(|e| e.id == id) {
            return Err("Chat is not in this project's history".into());
        }
        let previous = std::mem::replace(&mut self.index.active, id.into());
        if let Err(error) = self.save() {
            self.index.active = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn create(&mut self) -> Result<(), String> {
        if self.index.entries.len() >= 512 {
            return Err(
                "This project has 512 saved chats. Existing conversations were kept.".into(),
            );
        }
        let id = new_id();
        let previous = self.index.active.clone();
        self.index.entries.push(Entry {
            id: id.clone(),
            title: "New chat".into(),
        });
        self.index.active = id;
        if let Err(error) = self.save() {
            self.index.entries.pop();
            self.index.active = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn title(&mut self, title: &str) -> Result<(), String> {
        let title: String = title.chars().take(100).collect();
        if let Some(entry) = self
            .index
            .entries
            .iter_mut()
            .find(|e| e.id == self.index.active)
        {
            if entry.title == title {
                return Ok(());
            }
            entry.title = title;
        }
        self.save()
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn new_chat_retains_previous_and_catalog_reopens_selected_chat() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("prefs.json");
        let mut library = Library::open(&profile, "filmcraft", "project").unwrap();
        let old = library.active().to_owned();
        {
            let mut store = library.store(std::sync::Arc::new(|| {})).unwrap();
            store.queue(json!({"version":1,"text":"old goal"}));
        }
        library.create().unwrap();
        assert_ne!(old, library.active());
        library.select(&old).unwrap();
        drop(library);
        let mut reopened = Library::open(&profile, "filmcraft", "project").unwrap();
        assert_eq!(reopened.entries().len(), 2);
        assert_eq!(reopened.active(), old);
        let store = reopened.store(std::sync::Arc::new(|| {})).unwrap();
        assert_eq!(
            store.events.recv().unwrap().unwrap().unwrap()["text"],
            "old goal"
        );
        assert!(reopened.select("../../outside").is_err());
    }
}
