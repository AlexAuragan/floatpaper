//! Opt-in Hyprland visibility policy. IPC runs off the render thread.
use anyhow::{bail, Context, Result};
use calloop::channel::Sender;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason { Fullscreen, PoweredOff }
pub type HiddenOutputs = BTreeMap<String, Reason>;

#[derive(Deserialize)]
struct Workspace {
    name: String,
    id: Option<i64>,
    address: Option<String>,
}
impl Workspace {
    fn matches(&self, other: &Self) -> bool {
        if self.name.is_empty() || other.name.is_empty() { return false; }
        match (&self.address, &other.address) {
            (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a == b,
            _ => match (self.id, other.id) {
                (Some(a), Some(b)) => a == b && self.name == other.name,
                _ => self.name == other.name,
            },
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Monitor {
    id: i64,
    name: String,
    active_workspace: Workspace,
    special_workspace: Workspace,
    dpms_status: bool,
    disabled: bool,
}
#[derive(Deserialize)]
struct Client {
    monitor: i64,
    workspace: Workspace,
    mapped: bool,
    hidden: bool,
    fullscreen: u8,
    visible: Option<bool>,
}

fn classify(monitors: &[Monitor], clients: &[Client]) -> HiddenOutputs {
    let mut hidden = HiddenOutputs::new();
    for m in monitors {
        if !m.dpms_status || m.disabled {
            hidden.insert(m.name.clone(), Reason::PoweredOff);
        } else if m.special_workspace.name.is_empty() && clients.iter().any(|c|
            c.monitor == m.id && c.mapped && !c.hidden && c.visible != Some(false)
                && c.fullscreen == 2 && c.workspace.matches(&m.active_workspace)
        ) {
            // IPC does not expose actual surface opacity. Only enable this
            // policy when the user accepts that true fullscreen implies coverage.
            hidden.insert(m.name.clone(), Reason::Fullscreen);
        }
    }
    hidden
}

const IO_TIMEOUT: Duration = Duration::from_millis(500);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(150);
const RESYNC: Duration = Duration::from_secs(2);

fn query<T: serde::de::DeserializeOwned>(dir: &Path, request: &str) -> Result<T> {
    let mut socket = UnixStream::connect(dir.join(".socket.sock")).context("Connect to Hyprland command socket")?;
    socket.set_read_timeout(Some(IO_TIMEOUT))?;
    socket.set_write_timeout(Some(IO_TIMEOUT))?;
    socket.write_all(request.as_bytes())?;
    let mut bytes = Vec::new();
    // Limit unexpected responses; invalid/truncated JSON fails open.
    socket.take(8 * 1024 * 1024).read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).with_context(|| format!("Decode Hyprland {request}"))
}

fn snapshot(dir: &Path) -> Result<HiddenOutputs> {
    let monitors: Vec<Monitor> = query(dir, "j/monitors all")?;
    let clients: Vec<Client> = query(dir, "j/clients")?;
    Ok(classify(&monitors, &clients))
}

fn listen(dir: &Path, tx: &Sender<HiddenOutputs>, running: &AtomicBool) -> Result<()> {
    // Subscribe before the initial snapshot so transitions during startup
    // are not lost. Refresh on events rather than interpreting toggle payloads.
    let mut events = UnixStream::connect(dir.join(".socket2.sock")).context("Connect to Hyprland event socket")?;
    events.set_read_timeout(Some(EVENT_DEBOUNCE))?;
    let mut last = Instant::now();
    let mut dirty = false;
    if tx.send(snapshot(dir)?).is_err() { return Ok(()); }
    let mut buf = [0u8; 8192];
    while running.load(Ordering::SeqCst) {
        match events.read(&mut buf) {
            Ok(0) => bail!("Hyprland event socket disconnected"),
            Ok(_) => dirty = true,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {},
            Err(e) => return Err(e).context("Read Hyprland events"),
        }
        // Bound command traffic during event bursts. Periodic reconciliation
        // covers DPMS and state changes for which no event is emitted.
        if last.elapsed() >= RESYNC || (dirty && last.elapsed() >= EVENT_DEBOUNCE) {
            if tx.send(snapshot(dir)?).is_err() { return Ok(()); }
            last = Instant::now();
            dirty = false;
        }
    }
    Ok(())
}

pub fn start(tx: Sender<HiddenOutputs>, running: Arc<AtomicBool>) -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").context("Not a Hyprland session")?;
    let dir = PathBuf::from(runtime).join("hypr").join(signature);
    thread::Builder::new().name("hyprland-visibility".into()).spawn(move || {
        let mut last_error = None;
        while running.load(Ordering::SeqCst) {
            match listen(&dir, &tx, &running) {
                Ok(()) => break,
                Err(e) => {
                    // Never keep a stale suspension decision after IPC failure.
                    if tx.send(HiddenOutputs::new()).is_err() { break; }
                    let msg = format!("{e:#}");
                    if last_error.as_ref() != Some(&msg) {
                        eprintln!("Visibility IPC: {msg}; rendering all outputs until reconnect");
                        last_error = Some(msg);
                    }
                    for _ in 0..10 {
                        if !running.load(Ordering::SeqCst) { return; }
                        thread::sleep(Duration::from_millis(200));
                    }
                },
            }
        }
    }).context("Start Hyprland visibility worker")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitors() -> Vec<Monitor> {
        serde_json::from_str(r#"[
          {"id":0,"name":"DP-1","activeWorkspace":{"id":1,"name":"1"},"specialWorkspace":{"id":0,"name":""},"dpmsStatus":true,"disabled":false},
          {"id":1,"name":"DP-2","activeWorkspace":{"id":2,"name":"2"},"specialWorkspace":{"id":0,"name":""},"dpmsStatus":true,"disabled":false}
        ]"#).unwrap()
    }
    fn clients() -> Vec<Client> {
        serde_json::from_str(r#"[{"monitor":0,"workspace":{"id":1,"name":"1"},"mapped":true,"hidden":false,"fullscreen":2}]"#).unwrap()
    }
    #[test]
    fn only_matching_true_fullscreen_suspends() {
        let m = monitors();
        let mut c = clients();
        assert_eq!(classify(&m, &c), BTreeMap::from([("DP-1".into(), Reason::Fullscreen)]));
        c[0].fullscreen = 1;
        assert!(classify(&m, &c).is_empty());
        c[0].fullscreen = 2;
        c[0].hidden = true;
        assert!(classify(&m, &c).is_empty());
        c[0].hidden = false;
        c[0].workspace.id = Some(9);
        assert!(classify(&m, &c).is_empty());
        c[0].workspace.id = Some(1);
        c[0].visible = Some(false);
        assert!(classify(&m, &c).is_empty());
        c[0].visible = None;
        c[0].mapped = false;
        assert!(classify(&m, &c).is_empty());
    }
    #[test]
    fn special_workspace_and_dpms() {
        let mut m = monitors();
        m[0].special_workspace.name = "special:overlay".into();
        m[1].dpms_status = false;
        assert_eq!(classify(&m, &clients()), BTreeMap::from([("DP-2".into(), Reason::PoweredOff)]));
        m[0].special_workspace.name.clear();
        assert_eq!(classify(&m, &clients()).len(), 2);
        m[1].dpms_status = true;
        assert_eq!(classify(&m, &[]), HiddenOutputs::new());
    }
    #[test]
    fn modern_named_workspaces_without_numeric_ids() {
        let a: Workspace = serde_json::from_str(r#"{"name":"code","address":"name:code"}"#).unwrap();
        let b: Workspace = serde_json::from_str(r#"{"name":"code","address":"name:code"}"#).unwrap();
        let c: Workspace = serde_json::from_str(r#"{"name":"code","address":"name:other"}"#).unwrap();
        assert!(a.matches(&b));
        assert!(!a.matches(&c));
    }
}
