//! Persistent authorization of command declarations, scoped to a canonical scene path.
use crate::scene::{Scene, TextSource};
use anyhow::{bail, Context, Result};
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt}, path::{Path, PathBuf}};

type Commands = BTreeMap<String, Vec<String>>;
type Store = BTreeMap<PathBuf, Commands>;
const MAX_STORE: u64 = 4 * 1024 * 1024;

fn declarations(scene: &Scene) -> Commands {
    scene.text_sources.iter().filter_map(|(name, source)| match source {
        TextSource::Command { command, .. } => Some((name.clone(), command.clone())),
        _ => None,
    }).collect()
}

fn store_path() -> Result<PathBuf> {
    let root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?).join(".config"),
    };
    if !root.is_absolute() { bail!("Config directory must be absolute"); }
    Ok(root.join("floatpaper").join("trusted-scenes.json"))
}

fn open_store(path: &Path, create: bool, write: bool) -> Result<Option<File>> {
    let dir = path.parent().context("Missing trust directory")?;
    if create {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let uid = unsafe { libc::geteuid() };
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        bail!("Trust directory must be owned by you and private (0700): {}", dir.display());
    }
    let file = match OpenOptions::new().read(true).write(write).create(create)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(f) => f,
        Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        bail!("Trust file must be a private regular file owned by you");
    }
    Ok(Some(file))
}

fn read_store(file: &mut File) -> Result<Store> {
    let mut bytes = Vec::new();
    file.take(MAX_STORE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STORE { bail!("Trust store too large"); }
    if bytes.is_empty() { return Ok(Store::new()); }
    serde_json::from_slice(&bytes).context("Invalid trust store")
}

pub fn allowed(path: &Path, scene: &Scene) -> bool {
    let wanted = declarations(scene);
    if wanted.is_empty() { return false; }
    let check = || -> Result<bool> {
        let Some(mut file) = open_store(&store_path()?, false, false)? else { return Ok(false); };
        file.lock_shared()?;
        let store = read_store(&mut file)?;
        Ok(store.get(path) == Some(&wanted))
    };
    match check() {
        Ok(ok) => ok,
        Err(e) => { eprintln!("Cannot verify scene trust: {e:#}"); false }
    }
}

pub fn change(path: &Path, grant: bool) -> Result<()> {
    let path = path.canonicalize().context("Scene path does not exist")?;
    let wanted = if grant {
        let scene = Scene::load(&path)?;
        let commands = declarations(&scene);
        if commands.is_empty() { bail!("Scene has no command declarations"); }
        eprintln!("Authorizing commands for {} (including scripts and programs they invoke):", path.display());
        for (name, argv) in &commands { eprintln!("  {name}: {argv:?}"); }
        Some(commands)
    } else { None };
    // JSON paths must be representable as UTF-8; fail before modifying the store.
    if path.to_str().is_none() { bail!("Scene path must be UTF-8"); }
    let Some(mut file) = open_store(&store_path()?, grant, true)? else {
        println!("Scene is not trusted"); return Ok(());
    };
    // Exclusive lock serializes simultaneous CLI updates.
    file.lock()?;
    let mut store = read_store(&mut file)?;
    if let Some(commands) = wanted { store.insert(path.clone(), commands); }
    else { store.remove(&path); }
    let bytes = serde_json::to_vec_pretty(&store)?;
    if bytes.len() as u64 > MAX_STORE { bail!("Trust store too large"); }
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!("{}: {}", if grant { "Trusted" } else { "Revoked" }, path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_depends_on_command_names_and_argv_not_visuals() {
        let config = r#"
[text_sources.clock]
kind = "command"
command = ["date", "+%H:%M"]
interval = 1.0
timeout = 1.0
default = "--"
"#;
        let scene: Scene = toml::from_str(config).unwrap();
        let visual: Scene = toml::from_str(&format!("fps = 60\n{config}")).unwrap();
        let changed: Scene = toml::from_str(&config.replace("+%H:%M", "+%s")).unwrap();
        assert_eq!(declarations(&scene), declarations(&visual));
        assert_ne!(declarations(&scene), declarations(&changed));
    }
}
