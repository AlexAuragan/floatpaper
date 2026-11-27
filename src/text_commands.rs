//! One cancellable worker per named command, shared by all render groups.
use crate::scene::{Scene, TextSource};
use anyhow::{bail, Context, Result};
use std::{
    collections::BTreeMap, io::{Read, ErrorKind}, os::fd::AsRawFd,
    os::unix::process::CommandExt, path::{Path, PathBuf},
    process::{Command, Stdio, Child}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
    thread::{self, JoinHandle}, time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 16384;
const POLL: Duration = Duration::from_millis(25);
pub type Values = BTreeMap<String, String>;

pub struct Commands {
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    values: Arc<Mutex<Values>>,
    workers: Vec<JoinHandle<()>>,
}
impl Commands {
    pub fn new(scene: &Scene, base: &Path, allowed: bool) -> Result<Self> {
        let mut this = Self { active: Arc::new(AtomicBool::new(false)), stop: Arc::new(AtomicBool::new(false)),
            values: Arc::new(Mutex::new(BTreeMap::new())), workers: Vec::new() };
        if !allowed {
            if scene.has_commands() { eprintln!("Command text disabled: using defaults. Pass --allow-commands only for trusted scenes."); }
            return Ok(this);
        }
        for (name, source) in &scene.text_sources {
            let TextSource::Command { command, interval, timeout, .. } = source else { continue; };
            if !scene.layers.iter().any(|l| l.shapes.iter().any(|s| s.text_source.as_deref() == Some(name.as_str()))) { continue; }
            let name = name.clone();
            let command = command.clone();
            let interval = Duration::from_secs_f64(*interval);
            let timeout = Duration::from_secs_f64(*timeout);
            let base = base.to_path_buf();
            let active = this.active.clone();
            let stop = this.stop.clone();
            let values = this.values.clone();
            this.workers.push(thread::Builder::new().name("floatpaper-text".into()).spawn(move || {
                let mut due = Instant::now();
                let mut last_error = None;
                while !stop.load(Ordering::Acquire) {
                    if !active.load(Ordering::Acquire) {
                        due = Instant::now();
                        thread::sleep(POLL);
                        continue;
                    }
                    if Instant::now() < due { thread::sleep(POLL.min(due.saturating_duration_since(Instant::now()))); continue; }
                    let start = Instant::now();
                    let result = execute(&command, &base, timeout, || !active.load(Ordering::Acquire) || stop.load(Ordering::Acquire));
                    let mut map = values.lock().expect("command values mutex poisoned");
                    if !active.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
                        map.remove(&name);
                    } else {
                        match result {
                            Ok(value) => { map.insert(name.clone(), value); last_error = None; }
                            Err(e) => {
                                map.remove(&name);
                                let message = format!("{e:#}");
                                if last_error.as_ref() != Some(&message) { eprintln!("Text command {name}: {message}; using default"); }
                                last_error = Some(message);
                            }
                        }
                    }
                    drop(map);
                    // At most one child per source; missed intervals are not queued.
                    due = (start + interval).max(Instant::now());
                }
            }).context("Start text command worker")?);
        }
        Ok(this)
    }
    pub fn set_active(&self, active: bool) {
        let previous = self.active.swap(active, Ordering::AcqRel);
        if previous && !active { self.values.lock().expect("command values mutex poisoned").clear(); }
    }
    pub fn snapshot(&self) -> Values { self.values.lock().expect("command values mutex poisoned").clone() }
}
impl Drop for Commands {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.active.store(false, Ordering::Release);
        for worker in self.workers.drain(..) {
            if worker.join().is_err() { eprintln!("Text command worker panicked"); }
        }
    }
}

// Own the process group so scripts' ordinary subprocesses are cancelled too.
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        // The leader is kept unreaped until group termination, preventing PID reuse.
        unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL); }
        if let Err(e) = self.0.wait() { eprintln!("Reap text command: {e}"); }
    }
}
fn execute(command: &[String], base: &PathBuf, timeout: Duration, cancelled: impl Fn() -> bool) -> Result<String> {
    if cancelled() { bail!("cancelled"); }
    let start = Instant::now();
    let mut child = Running(Command::new(&command[0]).args(&command[1..]).current_dir(base)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .process_group(0).spawn().context("Spawn command")?);
    let mut stdout = child.0.stdout.take().context("Missing command stdout")?;
    let fd = stdout.as_raw_fd();
    // Nonblocking read: a child cannot hang the worker by keeping its pipe open.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("Set nonblocking command stdout");
    }
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        if cancelled() { bail!("cancelled"); }
        if start.elapsed() >= timeout { bail!("timed out after {:.2}s", timeout.as_secs_f64()); }
        match stdout.read(&mut buffer) {
            Ok(0) => {
                // Inspect exit status without reaping, so Drop can safely clean up
                // any remaining members of the original process group.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let status = unsafe { libc::waitid(libc::P_PID, child.0.id() as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
                if status < 0 { return Err(std::io::Error::last_os_error()).context("Inspect command exit status"); }
                if unsafe { info.si_pid() } != 0 {
                    if info.si_code != libc::CLD_EXITED || unsafe { info.si_status() } != 0 { bail!("command exited unsuccessfully"); }
                    break;
                }
                thread::sleep(POLL);
            }
            Ok(n) => {
                if output.len() + n > MAX_OUTPUT { bail!("stdout exceeds {MAX_OUTPUT} bytes"); }
                output.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => thread::sleep(POLL),
            Err(e) => return Err(e).context("Read command stdout"),
        }
    }
    let text = String::from_utf8(output).context("Command output is not UTF-8")?;
    // Strip only final line endings, retaining indentation and trailing spaces.
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_success_failure_and_timeout() {
        let base = PathBuf::from(".");
        let run = |args: &[&str], timeout| execute(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &base, timeout, || false);
        assert_eq!(run(&["printf", " 42 \\n"], Duration::from_secs(2)).unwrap(), " 42 ");
        assert!(run(&["false"], Duration::from_secs(2)).is_err());
        assert!(run(&["sleep", "5"], Duration::from_millis(100)).is_err());
    }
}
