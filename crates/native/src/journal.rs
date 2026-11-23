//! Durable goal checkpoints and immutable receipts. No editor edit precedes its pending record.
use craft_ai_chat_core::{ToolCall, goal::Goal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_RECORD: usize = 9 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checked {
    version: u32,
    sha256: String,
    data: Value,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|_| "Cannot create local chat storage")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect local chat storage")?;
    }
    Ok(())
}

pub(crate) fn lock(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| "Cannot open the chat storage lock")?;
    file.try_lock()
        .map_err(|_| "This chat or goal is busy in another editor instance")?;
    Ok(file)
}

pub(crate) fn read_checked(path: &Path, limit: usize) -> Result<Option<Value>, String> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read local chat checkpoint".into()),
    };
    let mut bytes = vec![];
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read local chat checkpoint")?;
    if bytes.len() > limit {
        return Err("Local chat checkpoint exceeds its size limit".into());
    }
    let record: Checked = serde_json::from_slice(&bytes)
        .map_err(|_| "Local chat checkpoint is damaged; its files were kept")?;
    if record.version != 1
        || record.sha256
            != digest(&serde_json::to_vec(&record.data).map_err(|_| "Invalid checkpoint data")?)
    {
        return Err("Local chat checkpoint checksum failed; its files were kept".into());
    }
    craft_ai_chat_core::bounds::value_size(&record.data, limit)?;
    Ok(Some(record.data))
}

pub(crate) fn write_checked(
    path: &Path,
    data: &Value,
    limit: usize,
    backup: bool,
) -> Result<(), String> {
    craft_ai_chat_core::bounds::value_size(data, limit.saturating_sub(256))?;
    let parent = path.parent().ok_or("Invalid checkpoint location")?;
    private_dir(parent)?;
    let record = Checked {
        version: 1,
        sha256: digest(&serde_json::to_vec(data).map_err(|_| "Cannot encode checkpoint")?),
        data: data.clone(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| "Cannot encode checkpoint")?;
    if bytes.len() > limit {
        return Err("Chat checkpoint exceeds its size limit".into());
    }
    let temporary = parent.join(format!(
        ".checkpoint-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary).map_err(
            |_| "Cannot write chat checkpoint; check storage permissions and free space",
        )?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "Cannot flush chat checkpoint; no further goal actions will run")?;
        drop(file);
        if backup && path.is_file() {
            // Only a verified previous checkpoint is eligible as a recovery copy.
            let previous = if read_checked(path, limit).is_ok() {
                path.with_extension("backup")
            } else {
                path.with_extension("damaged")
            };
            std::fs::copy(path, previous)
                .map_err(|_| "Cannot preserve the previous chat checkpoint")?;
        }
        std::fs::rename(&temporary, path).map_err(|_| "Cannot commit chat checkpoint")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub goal: String,
    pub sequence: u64,
    pub id: String,
    pub call: ToolCall,
    pub result: Value,
}

pub struct GoalStore {
    root: PathBuf,
    goal: String,
    _lease: File,
}
impl GoalStore {
    pub fn at(&self, sequence: u64) -> Result<Option<Receipt>, String> {
        self.sequence(sequence)
    }
    pub fn open(profile: &Path, host: &str, goal_id: &str) -> Result<Self, String> {
        if host.len() > 128
            || goal_id.is_empty()
            || goal_id.len() > 256
            || profile.as_os_str().len() > 32_768
        {
            return Err("Invalid goal storage identity".into());
        }
        let parent = profile.parent().ok_or("Cannot locate goal storage")?;
        let identity = serde_json::to_vec(&(
            profile
                .file_name()
                .ok_or("Invalid preferences profile")?
                .to_string_lossy(),
            host,
            goal_id,
        ))
        .map_err(|_| "Invalid goal identity")?;
        let root = parent
            .join("ai-chat-history")
            .join("goals")
            .join(digest(&identity));
        private_dir(&root)?;
        let lease = lock(&root.join("writer.lock"))?;
        Ok(Self {
            root,
            goal: goal_id.into(),
            _lease: lease,
        })
    }
    pub fn save(&self, goal: &Goal) -> Result<(), String> {
        goal.validate()?;
        if goal.id != self.goal {
            return Err("Goal storage identity mismatch".into());
        }
        if let Some(intent) = self.latest_intent()?
            && !goal.instructions.starts_with(&intent)
        {
            return Err("Refusing to overwrite newer user goal instructions".into());
        }
        let intent_path = self
            .root
            .join("intent")
            .join(format!("{:03}.json", goal.instructions.len()));
        let intent = serde_json::json!({"goal":goal.id,"instructions":goal.instructions});
        match read_checked(&intent_path, craft_ai_chat_core::goal::MAX_STATE + 256)? {
            Some(existing) if existing != intent => {
                return Err("User goal instructions are immutable".into());
            }
            Some(_) => {}
            None => write_checked(
                &intent_path,
                &intent,
                craft_ai_chat_core::goal::MAX_STATE + 256,
                false,
            )?,
        }
        write_checked(
            &self.root.join("state.json"),
            &serde_json::to_value(goal).map_err(|_| "Cannot encode goal")?,
            craft_ai_chat_core::goal::MAX_STATE + 256,
            true,
        )
    }
    pub fn load(&self) -> Result<Option<Goal>, String> {
        let path = self.root.join("state.json");
        let (value, recovered) =
            match read_checked(&path, craft_ai_chat_core::goal::MAX_STATE + 256) {
                Ok(Some(value)) => (Some(value), false),
                Ok(None) => (
                    read_checked(
                        &path.with_extension("backup"),
                        craft_ai_chat_core::goal::MAX_STATE + 256,
                    )?,
                    true,
                ),
                Err(primary) => match read_checked(
                    &path.with_extension("backup"),
                    craft_ai_chat_core::goal::MAX_STATE + 256,
                ) {
                    Ok(Some(value)) => (Some(value), true),
                    _ => return Err(primary),
                },
            };
        let Some(value) = value else {
            if self.latest_intent()?.is_some() {
                return Err("Goal instructions are preserved, but execution state needs recovery before resuming".into());
            }
            return Ok(None);
        };
        let mut goal: Goal = serde_json::from_value(value)
            .map_err(|_| "Invalid saved goal; the checkpoint was kept")?;
        goal.validate()?;
        if goal.id != self.goal {
            return Err("Goal storage identity mismatch".into());
        }
        if let Some(intent) = self.latest_intent()? {
            if !intent.starts_with(&goal.instructions) {
                return Err("Goal instruction revisions do not agree".into());
            }
            if intent != goal.instructions {
                goal.instructions = intent;
                goal.phase = craft_ai_chat_core::goal::Phase::Paused;
            }
        }
        for n in 0..=1024 {
            let sequence = goal
                .completed
                .checked_add(1)
                .ok_or("Goal receipt counter exhausted")?;
            let Some(receipt) = self.sequence(sequence)? else {
                break;
            };
            if n == 1024 {
                return Err(
                    "Goal has a large recovery tail; all receipts were preserved for review".into(),
                );
            }
            if let Some(pending) = &goal.pending {
                if pending.id != receipt.id
                    || pending.call.name != receipt.call.name
                    || pending.call.arguments != receipt.call.arguments
                {
                    return Err("Goal receipt does not match its pending action".into());
                }
            } else {
                goal.pending = Some(craft_ai_chat_core::goal::Step {
                    id: receipt.id.clone(),
                    call: receipt.call.clone(),
                });
            }
            goal.after(&receipt.id, &receipt.result)?;
            self.index_receipt(&receipt)?;
        }
        if recovered && goal.phase == craft_ai_chat_core::goal::Phase::Complete {
            goal.phase = craft_ai_chat_core::goal::Phase::Paused;
        }
        goal.needs_review |= recovered;
        goal.restored();
        Ok(Some(goal))
    }
    pub fn receipt(&self, goal: &str, id: &str) -> Result<Option<Receipt>, String> {
        if goal != self.goal || id.is_empty() || id.len() > 256 {
            return Err("Invalid goal receipt identity".into());
        }
        let path = self
            .root
            .join("lookup")
            .join(format!("{}.json", digest(id.as_bytes())));
        let Some(value) = read_checked(&path, MAX_RECORD)? else {
            return Ok(None);
        };
        let sequence = value["sequence"]
            .as_u64()
            .ok_or("Invalid goal receipt index")?;
        let receipt = self
            .sequence(sequence)?
            .ok_or("Goal receipt index has no durable result")?;
        if receipt.goal != goal || receipt.id != id {
            return Err("Goal receipt identity mismatch".into());
        }
        Ok(Some(receipt))
    }
    pub fn record(&self, receipt: &Receipt) -> Result<(), String> {
        if receipt.goal != self.goal
            || receipt.sequence == 0
            || receipt.sequence > 100_000
            || receipt.id.is_empty()
            || receipt.id.len() > 256
        {
            return Err("Invalid goal receipt identity or quota exceeded".into());
        }
        let path = self
            .root
            .join("receipts")
            .join(format!("{:020}.json", receipt.sequence));
        if let Some(existing) = self.receipt(&receipt.goal, &receipt.id)? {
            if existing.call.name != receipt.call.name
                || existing.sequence != receipt.sequence
                || existing.call.arguments != receipt.call.arguments
                || existing.result != receipt.result
            {
                return Err("Refusing to overwrite a completed goal action".into());
            }
            return Ok(());
        }
        if let Some(existing) = self.sequence(receipt.sequence)? {
            if existing.id != receipt.id
                || existing.call.name != receipt.call.name
                || existing.call.arguments != receipt.call.arguments
                || existing.result != receipt.result
            {
                return Err("Refusing to replace another confirmed goal operation".into());
            }
        } else {
            write_checked(
                &path,
                &serde_json::to_value(receipt).map_err(|_| "Cannot encode goal receipt")?,
                MAX_RECORD,
                false,
            )?;
        }
        self.index_receipt(receipt)
    }
    fn sequence(&self, sequence: u64) -> Result<Option<Receipt>, String> {
        if sequence == 0 || sequence > 100_000 {
            return Err("Invalid goal receipt sequence".into());
        }
        let path = self
            .root
            .join("receipts")
            .join(format!("{sequence:020}.json"));
        let Some(value) = read_checked(&path, MAX_RECORD)? else {
            return Ok(None);
        };
        let receipt: Receipt = serde_json::from_value(value).map_err(|_| "Invalid goal receipt")?;
        if receipt.goal != self.goal || receipt.sequence != sequence {
            return Err("Goal receipt scope mismatch".into());
        }
        Ok(Some(receipt))
    }
    fn index_receipt(&self, receipt: &Receipt) -> Result<(), String> {
        let path = self
            .root
            .join("lookup")
            .join(format!("{}.json", digest(receipt.id.as_bytes())));
        let value = serde_json::json!({"sequence":receipt.sequence});
        if let Some(existing) = read_checked(&path, 4096)? {
            if existing != value {
                return Err("Refusing to change a goal receipt identity".into());
            }
            return Ok(());
        }
        write_checked(&path, &value, 4096, false)
    }
    fn latest_intent(&self) -> Result<Option<Vec<String>>, String> {
        for revision in (1..=64).rev() {
            let path = self.root.join("intent").join(format!("{revision:03}.json"));
            if let Some(value) = read_checked(&path, craft_ai_chat_core::goal::MAX_STATE + 256)? {
                let instructions: Vec<String> =
                    serde_json::from_value(value["instructions"].clone())
                        .map_err(|_| "Invalid canonical goal instructions")?;
                if value["goal"] != self.goal || instructions.len() != revision {
                    return Err("Goal instruction scope mismatch".into());
                }
                return Ok(Some(instructions));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn goal() -> Goal {
        Goal::new(
            "g1".into(),
            "project".into(),
            "Rename all 70 tracks; keep extensions.".into(),
        )
        .unwrap()
    }
    #[test]
    fn pending_work_survives_crash_and_a_durable_receipt_resolves_it() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("prefs.json");
        let mut goal = goal();
        let call = ToolCall::new("edit", json!({"id":42,"name":"Take 01"}));
        {
            let store = GoalStore::open(&profile, "test", &goal.id).unwrap();
            store.save(&goal).unwrap();
            goal.before("call-1", &call).unwrap();
            store.save(&goal).unwrap();
        }
        {
            let store = GoalStore::open(&profile, "test", &goal.id).unwrap();
            let loaded = store.load().unwrap().unwrap();
            assert!(loaded.needs_review);
            assert_eq!(loaded.pending.as_ref().unwrap().id, "call-1");
            store
                .record(&Receipt {
                    goal: goal.id.clone(),
                    sequence: 1,
                    id: "call-1".into(),
                    call: call.clone(),
                    result: json!({"ok":true,"renamed":42}),
                })
                .unwrap();
        }
        let store = GoalStore::open(&profile, "test", &goal.id).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.completed, 1);
        assert!(loaded.pending.is_none());
        assert_eq!(loaded.phase, craft_ai_chat_core::goal::Phase::Paused);
        assert_eq!(loaded.instructions, goal.instructions);
    }
    #[test]
    fn receipts_are_immutable_and_a_second_writer_cannot_execute_the_same_goal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prefs.json");
        let store = GoalStore::open(&p, "test", "g1").unwrap();
        assert!(GoalStore::open(&p, "test", "g1").is_err());
        let mut receipt = Receipt {
            goal: "g1".into(),
            sequence: 1,
            id: "call-1".into(),
            call: ToolCall::new("edit", json!({"id":42})),
            result: json!({"ok":true}),
        };
        store.record(&receipt).unwrap();
        store.record(&receipt).unwrap();
        receipt.result = json!({"ok":false});
        assert!(store.record(&receipt).is_err());
        drop(store);
        assert!(GoalStore::open(&p, "test", "g1").is_ok());
    }
    #[test]
    fn damaged_state_recovers_paused_without_trusting_an_older_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let store = GoalStore::open(&dir.path().join("prefs.json"), "test", "g1").unwrap();
        let mut goal = goal();
        store.save(&goal).unwrap();
        goal.steer("Do not alter the MIDI notes.".into()).unwrap();
        store.save(&goal).unwrap();
        std::fs::write(store.root.join("state.json"), b"truncated").unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert!(loaded.needs_review);
        assert_eq!(loaded.phase, craft_ai_chat_core::goal::Phase::Paused);
        assert!(store.root.join("state.json").exists());
        assert_eq!(
            loaded.instructions, goal.instructions,
            "a corrupt state file must not lose newer user criteria"
        );
    }
}
