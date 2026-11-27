//! Local runtime commands for one controlled live instance per Wayland display.
use anyhow::{bail, Context, Result};
use calloop::channel::Sender;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::FileTypeExt, net::{UnixListener, UnixStream}},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command { Pause, Resume, Toggle, Status }

impl Command {
    fn wire(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Toggle => "toggle",
            Self::Status => "status",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "pause" => Ok(Self::Pause),
            "resume" => Ok(Self::Resume),
            "toggle" => Ok(Self::Toggle),
            "status" => Ok(Self::Status),
            _ => bail!("Unknown control command"),
        }
    }
}

pub struct Clock {
    start: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl Clock {
    pub fn new(now: Instant) -> Self {
        Self { start: now, paused_at: None, paused_total: Duration::ZERO }
    }

    pub fn paused(&self) -> bool { self.paused_at.is_some() }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.paused_at.unwrap_or(now).saturating_duration_since(self.start)
            .saturating_sub(self.paused_total)
    }

    pub fn apply(&mut self, command: Command, now: Instant) {
        let pause = match command {
            Command::Pause => true,
            Command::Resume => false,
            Command::Toggle => !self.paused(),
            Command::Status => return,
        };
        if pause && !self.paused() {
            self.paused_at = Some(now);
        } else if !pause {
            if let Some(at) = self.paused_at.take() {
                self.paused_total += now.saturating_duration_since(at);
            }
        }
    }

    pub fn status(&self) -> &'static str {
        if self.paused() { "paused" } else { "running" }
    }
}

pub struct Request {
    pub command: Command,
    pub reply: mpsc::Sender<&'static str>,
}

fn socket_path() -> Result<PathBuf> {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?);
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    let display = std::path::Path::new(&display).file_name().context("Invalid WAYLAND_DISPLAY")?;
    let mut name = std::ffi::OsString::from("floatpaper-");
    name.push(display);
    name.push(".sock");
    Ok(runtime.join(name))
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    // Held until after the socket is unlinked in Drop. The lock file itself
    // is intentionally retained so a restart never races a second inode.
    _lock: File,
}

impl Server {
    pub fn bind() -> Result<Self> {
        let path = socket_path()?;
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(path.with_extension("lock")).context("Open Floatpaper control lock")?;
        lock.try_lock().map_err(|e| anyhow::anyhow!(
            "Cannot acquire Floatpaper control lock (another controlled instance may be running): {e}"))?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() => fs::remove_file(&path).context("Remove stale control socket")?,
            Ok(_) => bail!("Control path exists and is not a socket: {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e).context("Inspect control socket"),
        }
        let listener = UnixListener::bind(&path).context("Bind Floatpaper control socket")?;
        Ok(Self { listener, path, _lock: lock })
    }

    pub fn start(&self, tx: Sender<Request>) -> Result<()> {
        let listener = self.listener.try_clone()?;
        thread::Builder::new().name("floatpaper-control".into()).spawn(move || {
            // Block in accept while idle: no extra timer or polling loop.
            for incoming in listener.incoming() {
                let mut stream = match incoming {
                    Ok(stream) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => { eprintln!("Control accept: {e}"); break; },
                };
                if let Err(e) = handle(&mut stream, &tx) {
                    eprintln!("Control request: {e:#}");
                    if let Err(e) = stream.write_all(b"error\n") {
                        eprintln!("Control error reply: {e}");
                    }
                }
            }
        }).context("Start control socket worker")?;
        eprintln!("Runtime control: {}", self.path.display());
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Remove control socket: {e}");
            }
        }
    }
}

fn handle(stream: &mut UnixStream, tx: &Sender<Request>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut line = String::new();
    BufReader::new(&mut *stream).take(64).read_line(&mut line)?;
    if !line.ends_with('\n') { bail!("Incomplete control command"); }
    let command = Command::parse(line.trim_end())?;
    let (reply, response) = mpsc::channel();
    tx.send(Request { command, reply }).map_err(|_| anyhow::anyhow!("Render loop has stopped"))?;
    let status = response.recv_timeout(Duration::from_secs(2)).context("Render loop did not acknowledge command")?;
    writeln!(stream, "{status}")?;
    Ok(())
}

pub fn send(command: Command) -> Result<()> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("Cannot connect to a controlled live Floatpaper instance at {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    writeln!(stream, "{}", command.wire())?;
    let mut line = String::new();
    BufReader::new(stream).take(64).read_line(&mut line)?;
    match line.as_str() {
        "paused\n" | "running\n" => print!("{line}"),
        _ => bail!("Control command failed: {}", line.trim_end()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_pause_freezes_clock_and_resume_is_continuous() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let mut clock = Clock::new(start);
        clock.apply(Command::Pause, at(3));
        clock.apply(Command::Pause, at(4));
        assert_eq!(clock.status(), "paused");
        assert_eq!(clock.elapsed(at(10)), Duration::from_secs(3));
        clock.apply(Command::Resume, at(10));
        clock.apply(Command::Resume, at(11));
        assert_eq!(clock.status(), "running");
        assert_eq!(clock.elapsed(at(12)), Duration::from_secs(5));
        clock.apply(Command::Toggle, at(12));
        clock.apply(Command::Status, at(14));
        assert_eq!(clock.elapsed(at(15)), Duration::from_secs(5));
        clock.apply(Command::Toggle, at(15));
        assert_eq!(clock.elapsed(at(16)), Duration::from_secs(6));
    }

    #[test]
    fn control_protocol_accepts_only_known_commands() {
        for command in [Command::Pause, Command::Resume, Command::Toggle, Command::Status] {
            assert_eq!(Command::parse(command.wire()).unwrap(), command);
        }
        assert!(Command::parse("quit").is_err());
    }
}
