//! Trusted-repository, hash-verified component delivery. Runs on a worker, never the UI thread.
use crate::Wake;
use craft_ai_chat_core::runtime::PROTOCOL;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

const REPOSITORY: &str = "https://github.com/lakam99/craft-ai-chat/releases";
const MAX_BINARY: u64 = 64 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol: u32,
    pub version: String,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("craft-ai-chat-runtime-windows-x64.exe"),
        ("linux", "x86_64") => Some("craft-ai-chat-runtime-linux-x64"),
        ("macos", "aarch64") => Some("craft-ai-chat-runtime-macos-arm64"),
        ("macos", "x86_64") => Some("craft-ai-chat-runtime-macos-x64"),
        _ => None,
    }
}
fn root() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("ArkCraft/components/chat-v1"))
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.join(".local/share/arkcraft/components/chat-v1"))
    }
}
fn valid_version(version: &str) -> bool {
    version.len() <= 32
        && version.split('.').count() == 3
        && version
            .split('.')
            .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}
impl Manifest {
    pub fn asset(&self, name: &str) -> Result<&Asset, String> {
        if self.protocol != PROTOCOL || !valid_version(&self.version) || self.assets.len() > 16 {
            return Err("No compatible chat component is available for this editor".into());
        }
        let mut found = self.assets.iter().filter(|a| a.name == name);
        let asset = found
            .next()
            .ok_or("This chat release does not contain a component for your OS")?;
        if found.next().is_some()
            || asset.size == 0
            || asset.size > MAX_BINARY
            || asset.sha256.len() != 64
            || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid chat component checksum manifest".into());
        }
        Ok(asset)
    }
}
fn verify(bytes: &[u8], asset: &Asset) -> Result<(), String> {
    if bytes.len() as u64 != asset.size
        || format!("{:x}", Sha256::digest(bytes)) != asset.sha256.to_ascii_lowercase()
    {
        return Err("Chat component verification failed; the installed version was kept".into());
    }
    Ok(())
}
fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "Cannot open chat component metadata")?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read chat component metadata")?;
    if bytes.len() as u64 > limit {
        return Err("Chat component file exceeds its size limit".into());
    }
    Ok(bytes)
}
/// Resolve only fixed filenames beneath our own versioned cache or beside the editor.
pub fn installed() -> Option<PathBuf> {
    candidates().into_iter().next()
}
/// Current, previous known-good, then the editor's bundled component.
pub fn candidates() -> Vec<PathBuf> {
    let Some(name) = platform() else {
        return vec![];
    };
    let mut paths = Vec::new();
    if let Some(root) = root() {
        for pointer in ["current.json", "previous.json"] {
            let bytes = std::fs::File::open(root.join(pointer))
                .ok()
                .and_then(|file| {
                    let mut bytes = Vec::new();
                    file.take(16 * 1024 + 1).read_to_end(&mut bytes).ok()?;
                    (bytes.len() <= 16 * 1024).then_some(bytes)
                });
            if let Some(manifest) = bytes.and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
                && manifest.asset(name).is_ok()
            {
                let path = root.join(&manifest.version).join(name);
                if path.is_file() && !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    if let Some(parent) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let path = parent.join(name);
        if path.is_file() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(Arc::new(rustls_rustcrypto::provider()))
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(90)))
        .build()
        .into()
}
fn fetch(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .header("User-Agent", "ArkCraft-chat-component")
        .call()
        .map_err(
            |_| "Could not download the chat component. Your current chat remains available.",
        )?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Chat component download was interrupted")?;
    if bytes.len() as u64 > limit {
        return Err("Chat download exceeds the size limit".into());
    }
    Ok(bytes)
}
fn probe(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(path);
    command
        .arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot validate the downloaded chat component")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err("The downloaded chat component could not start".into()),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Chat compatibility check timed out".into());
            }
        }
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or("Missing chat compatibility response")?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "Invalid chat compatibility response")?;
    if bytes.len() > 4096 {
        return Err("Oversized chat compatibility response".into());
    }
    let packet = serde_json::from_slice::<craft_ai_chat_core::runtime::Output>(&bytes)
        .map_err(|_| "Invalid chat compatibility response")?;
    if !matches!(
        packet,
        craft_ai_chat_core::runtime::Output::Ready {
            protocol: PROTOCOL,
            capabilities,
            ..
        } if capabilities.iter().any(|v|v==craft_ai_chat_core::runtime::SEND_RECEIPTS)
    ) {
        return Err("The downloaded chat component requires a different editor bridge".into());
    }
    Ok(())
}
fn install() -> Result<String, String> {
    let name = platform().ok_or("Chat component updates are not yet packaged for this platform")?;
    let root = root().ok_or("Cannot locate your private component folder")?;
    let agent = agent();
    let bytes = fetch(
        &agent,
        &format!("{REPOSITORY}/latest/download/chat-component-v1.json"),
        64 * 1024,
    )?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid chat release manifest")?;
    let asset = manifest.asset(name)?;
    let folder = root.join(&manifest.version);
    let path = folder.join(name);
    if path.is_file()
        && let Ok(bytes) = read_file(&path, asset.size)
        && verify(&bytes, asset).is_ok()
    {
        probe(&path)?;
        activate(&root, &bytes)?;
        return Ok(format!(
            "Chat {} is installed. Close and reopen the editor to use it.",
            manifest.version
        ));
    }
    let binary = fetch(
        &agent,
        &format!("{REPOSITORY}/download/v{}/{name}", manifest.version),
        asset.size,
    )?;
    verify(&binary, asset)?;
    std::fs::create_dir_all(&folder).map_err(|_| "Cannot create the chat component folder")?;
    // A separate candidate is verified and probed before any activation pointer is touched.
    let candidate = folder.join(format!("candidate-{}-{}", std::process::id(), name));
    std::fs::write(&candidate, binary)
        .map_err(|_| "Cannot write the chat component; check free disk space")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot make the chat component executable")?;
    }
    probe(&candidate)?;
    if path.exists() {
        return Err("A different component already occupies this version; your installed component was kept".into());
    }
    std::fs::rename(&candidate, &path)
        .map_err(|_| "Cannot finish installing the chat component")?;
    activate(&root, &bytes)?;
    Ok(format!(
        "Chat {} installed. Restart the editor when convenient; the current conversation is untouched.",
        manifest.version
    ))
}
fn activate(root: &Path, bytes: &[u8]) -> Result<(), String> {
    let current = root.join("current.json");
    if read_file(&current, 64 * 1024).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    let pointer = root.join(format!("current-{}.json", std::process::id()));
    std::fs::write(&pointer, bytes)
        .map_err(|_| "Cannot write the chat component activation record")?;
    // Keep one validated previous activation for recovery; never overwrite a running executable.
    let previous = root.join("previous.json");
    if current.exists() {
        std::fs::copy(&current, &previous)
            .map_err(|_| "Cannot preserve the previous chat component")?;
    }
    std::fs::rename(pointer, current).map_err(|_| "Cannot activate the new chat component")?;
    Ok(())
}
#[derive(Default)]
pub struct Updater {
    receive: Option<mpsc::Receiver<Result<String, String>>>,
    message: String,
}
impl Updater {
    pub fn busy(&self) -> bool {
        self.receive.is_some()
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn check(&mut self, wake: Wake) {
        if self.receive.is_some() {
            return;
        }
        self.message = "Checking for an independent chat update…".into();
        let (send, receive) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("chat-component-update".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(install).unwrap_or_else(|_| {
                    Err("Chat update worker stopped; the installed version was kept".into())
                });
                let _ = send.send(result);
                wake();
            }) {
            Ok(_) => self.receive = Some(receive),
            Err(_) => self.message = "Cannot start chat update worker".into(),
        }
    }
    pub fn poll(&mut self) {
        if let Some(result) = self.receive.as_ref().and_then(|r| r.try_recv().ok()) {
            self.receive = None;
            self.message = result.unwrap_or_else(|e| e);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_keeps_the_previous_version_and_repeated_checks_do_not_replace_it() {
        let dir = tempfile::tempdir().unwrap();
        activate(dir.path(), b"first").unwrap();
        activate(dir.path(), b"second").unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("current.json")).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(dir.path().join("previous.json")).unwrap(),
            b"first"
        );
        activate(dir.path(), b"second").unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("previous.json")).unwrap(),
            b"first"
        );
    }
    #[test]
    fn update_manifest_rejects_traversal_wrong_protocol_duplicates_and_corruption() {
        let asset = Asset {
            name: "component".into(),
            size: 3,
            sha256: format!("{:x}", Sha256::digest(b"abc")),
        };
        let mut manifest = Manifest {
            version: "0.6.0".into(),
            protocol: PROTOCOL,
            assets: vec![asset.clone()],
        };
        assert!(manifest.asset("component").is_ok());
        assert!(verify(b"abc", &asset).is_ok());
        assert!(verify(b"abd", &asset).is_err());
        manifest.version = "../escape".into();
        assert!(manifest.asset("component").is_err());
        manifest.version = "0.6.0".into();
        manifest.protocol += 1;
        assert!(manifest.asset("component").is_err());
        manifest.protocol = PROTOCOL;
        manifest.assets.push(asset);
        assert!(manifest.asset("component").is_err());
    }
}
