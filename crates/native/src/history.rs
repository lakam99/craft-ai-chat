//! Local, bounded chat archives. API credentials are never part of this format.
use crate::Wake;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};
pub const MAX_ARCHIVE: usize = 8 * 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
pub struct Store {
    sender: Option<mpsc::SyncSender<()>>,
    pub events: mpsc::Receiver<Result<Option<Value>, String>>,
    pending: Arc<Mutex<Option<Value>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Store {
    pub fn open(profile: &Path, host: &str, document: &str, wake: Wake) -> Result<Self, String> {
        let parent = profile
            .parent()
            .ok_or("Cannot locate chat history folder")?;
        let identity = format!(
            "{}\n{host}\n{document}",
            profile
                .file_name()
                .ok_or("Invalid preferences profile")?
                .to_string_lossy()
        );
        let path = parent
            .join("ai-chat-history")
            .join(format!("{:x}.json", Sha256::digest(identity.as_bytes())));
        let (sender, receive) = mpsc::sync_channel::<()>(1);
        let pending = Arc::new(Mutex::new(None::<Value>));
        let queued = pending.clone();
        let (events, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("craft-chat-history".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if events.send(load(&path)).is_err() {
                        return;
                    }
                    wake();
                    while receive.recv().is_ok() {
                        let Some(value) =
                            queued.lock().unwrap_or_else(PoisonError::into_inner).take()
                        else {
                            continue;
                        };
                        if let Err(error) = save(&path, &value) {
                            let _ = events.send(Err(error));
                            wake();
                        }
                    }
                }));
                if result.is_err() {
                    let _ = events.send(Err(
                        "Chat history worker failed; the editor remains open".into()
                    ));
                    wake();
                }
            })
            .map_err(|_| "Cannot start chat history storage")?;
        Ok(Self {
            sender: Some(sender),
            events: receiver,
            pending,
            worker: Some(worker),
        })
    }
    pub fn queue(&mut self, value: Value) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        self.flush();
    }
    pub fn flush(&mut self) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        // Drain the last queued snapshot when the editor closes or switches archive scopes.
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn load(path: &Path) -> Result<Option<Value>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read local chat history".into()),
    };
    let mut bytes = vec![];
    file.take((MAX_ARCHIVE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read local chat history")?;
    if bytes.len() > MAX_ARCHIVE {
        return Err("Local chat history exceeds its size limit".into());
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| "Local chat history is invalid")?;
    craft_ai_chat_core::bounds::value_size(&value, MAX_ARCHIVE)?;
    Ok(Some(value))
}
fn save(path: &Path, value: &Value) -> Result<(), String> {
    craft_ai_chat_core::bounds::value_size(value, MAX_ARCHIVE)?;
    let parent = path.parent().ok_or("Invalid chat history location")?;
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create local chat history folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect chat history folder")?;
    }
    let temporary: PathBuf = parent.join(format!(
        ".chat-{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Cannot save local chat history")?;
        serde_json::to_writer(&mut file, value).map_err(|_| "Cannot encode local chat history")?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|_| "Cannot flush local chat history")?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|_| "Cannot replace local chat history")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archives_replace_atomically_and_reject_corruption_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.json");
        save(&path, &serde_json::json!({"version":1,"text":"first"})).unwrap();
        save(&path, &serde_json::json!({"version":1,"text":"second"})).unwrap();
        assert_eq!(load(&path).unwrap().unwrap()["text"], "second");
        std::fs::write(&path, b"{bad").unwrap();
        assert!(load(&path).is_err());
        std::fs::File::create(&path)
            .unwrap()
            .set_len((MAX_ARCHIVE + 1) as u64)
            .unwrap();
        assert!(load(&path).is_err());
    }
}
