//! Process registry — JSON-backed supervisor child-process index (port of
//! `src/supervisor/process-registry.ts`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use thiserror::Error;

const REAP_SESSION_SIGTERM_TIMEOUT: Duration = Duration::from_secs(5);
const REAP_SESSION_SIGKILL_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum ProcessRegistryError {
    #[error("process registry I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("process registry JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ProcessRegistryError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionId {
    String(String),
    Number(i64),
}

impl SessionId {
    fn normalized(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => value.to_string(),
        }
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<i64> for SessionId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<i32> for SessionId {
    fn from(value: i32) -> Self {
        Self::Number(i64::from(value))
    }
}

impl From<u32> for SessionId {
    fn from(value: u32) -> Self {
        Self::Number(i64::from(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedProcessInfo {
    pub pid: i64,
    #[serde(rename = "type")]
    pub process_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProcessRecord {
    pub id: String,
    pub pid: i64,
    pub process_type: String,
    pub session_id: Option<SessionId>,
    pub started_at: String,
}

impl ManagedProcessRecord {
    fn from_entry(id: &str, info: &ManagedProcessInfo) -> Self {
        Self {
            id: id.to_owned(),
            pid: info.pid,
            process_type: info.process_type.clone(),
            session_id: info.session_id.clone(),
            started_at: info.started_at.clone(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedRegistry {
    #[serde(default)]
    processes: BTreeMap<String, ManagedProcessInfo>,
}

#[derive(Debug)]
pub struct ProcessRegistry {
    registry_path: PathBuf,
    entries: BTreeMap<String, ManagedProcessInfo>,
    initialized: bool,
}

impl ProcessRegistry {
    pub fn new<P: Into<PathBuf>>(registry_path: P) -> Self {
        Self {
            registry_path: registry_path.into(),
            entries: BTreeMap::new(),
            initialized: false,
        }
    }

    pub fn default_path() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".claude-mem").join("supervisor.json")
    }

    pub fn with_default_path() -> Self {
        Self::new(Self::default_path())
    }

    pub fn initialize(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;

        ensure_parent_dir(&self.registry_path)?;

        if !self.registry_path.exists() {
            self.persist()?;
            return Ok(());
        }

        match std::fs::read_to_string(&self.registry_path)
            .ok()
            .and_then(|text| serde_json::from_str::<PersistedRegistry>(&text).ok())
        {
            Some(raw) => {
                self.entries = raw.processes;
            }
            None => {
                self.entries.clear();
            }
        }

        self.prune_dead_entries()?;
        self.persist()?;
        Ok(())
    }

    pub fn register(&mut self, id: impl Into<String>, info: ManagedProcessInfo) -> Result<()> {
        self.initialize()?;
        self.entries.insert(id.into(), info);
        self.persist()
    }

    pub fn unregister(&mut self, id: &str) -> Result<()> {
        self.initialize()?;
        self.entries.remove(id);
        self.persist()
    }

    pub fn clear(&mut self) -> Result<()> {
        self.initialize()?;
        self.entries.clear();
        self.persist()
    }

    pub fn get_all(&mut self) -> Result<Vec<ManagedProcessRecord>> {
        self.initialize()?;
        Ok(self.sorted_records())
    }

    pub fn get_by_session(
        &mut self,
        session_id: impl Into<SessionId>,
    ) -> Result<Vec<ManagedProcessRecord>> {
        self.initialize()?;
        let normalized = session_id.into().normalized();
        Ok(self
            .sorted_records()
            .into_iter()
            .filter(|record| {
                record
                    .session_id
                    .as_ref()
                    .is_some_and(|session_id| session_id.normalized() == normalized)
            })
            .collect())
    }

    pub fn get_by_pid(&mut self, pid: i64) -> Result<Vec<ManagedProcessRecord>> {
        self.initialize()?;
        Ok(self
            .sorted_records()
            .into_iter()
            .filter(|record| record.pid == pid)
            .collect())
    }

    pub fn prune_dead_entries(&mut self) -> Result<usize> {
        self.initialize()?;

        let before = self.entries.len();
        self.entries.retain(|_, info| is_pid_alive(info.pid));
        let removed = before - self.entries.len();

        if removed > 0 {
            self.persist()?;
        }

        Ok(removed)
    }

    pub async fn reap_session(&mut self, session_id: impl Into<SessionId>) -> Result<usize> {
        self.initialize()?;
        let normalized = session_id.into().normalized();
        let session_records: Vec<_> = self
            .sorted_records()
            .into_iter()
            .filter(|record| {
                record
                    .session_id
                    .as_ref()
                    .is_some_and(|session_id| session_id.normalized() == normalized)
            })
            .collect();

        if session_records.is_empty() {
            return Ok(0);
        }

        let alive_records: Vec<_> = session_records
            .iter()
            .filter(|record| is_pid_alive(record.pid))
            .cloned()
            .collect();

        for record in &alive_records {
            send_signal(record.pid, Signal::Term);
        }

        wait_until_dead(&alive_records, REAP_SESSION_SIGTERM_TIMEOUT).await;

        let survivors: Vec<_> = alive_records
            .into_iter()
            .filter(|record| is_pid_alive(record.pid))
            .collect();

        for record in &survivors {
            send_signal(record.pid, Signal::Kill);
        }

        if !survivors.is_empty() {
            wait_until_dead(&survivors, REAP_SESSION_SIGKILL_TIMEOUT).await;
        }

        let reaped = session_records.len();
        for record in session_records {
            self.entries.remove(&record.id);
        }
        self.persist()?;

        Ok(reaped)
    }

    fn sorted_records(&self) -> Vec<ManagedProcessRecord> {
        let mut records: Vec<_> = self
            .entries
            .iter()
            .map(|(id, info)| ManagedProcessRecord::from_entry(id, info))
            .collect();
        records.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        records
    }

    fn persist(&self) -> Result<()> {
        let payload = PersistedRegistry {
            processes: self.entries.clone(),
        };
        ensure_parent_dir(&self.registry_path)?;
        std::fs::write(&self.registry_path, serde_json::to_string_pretty(&payload)?)?;
        Ok(())
    }
}

pub fn create_process_registry<P: Into<PathBuf>>(registry_path: P) -> ProcessRegistry {
    ProcessRegistry::new(registry_path)
}

static REGISTRY_SINGLETON: OnceLock<Mutex<ProcessRegistry>> = OnceLock::new();

pub fn get_process_registry() -> &'static Mutex<ProcessRegistry> {
    REGISTRY_SINGLETON.get_or_init(|| Mutex::new(ProcessRegistry::with_default_path()))
}

pub fn is_pid_alive(pid: i64) -> bool {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return false;
    }
    is_pid_alive_platform(pid as i32)
}

#[cfg(unix)]
fn is_pid_alive_platform(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn is_pid_alive_platform(pid: i32) -> bool {
    pid == std::process::id() as i32
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn send_signal(pid: i64, signal: Signal) {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return;
    }
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    let _ = unsafe { libc::kill(pid as i32, sig) };
}

#[cfg(not(unix))]
fn send_signal(_pid: i64, _signal: Signal) {}

async fn wait_until_dead(records: &[ManagedProcessRecord], timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if records.iter().all(|record| !is_pid_alive(record.pid)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
