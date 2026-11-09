//! Shared activation transaction for the shell and PowerShell installers.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zg_cli::StandaloneArgs;
use zg_daemon::{InstallationDaemon, ServerConfig};

mod launcher;
use launcher::Launcher;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const OWNER: &str = "zvec-grep standalone v1\n";
const MANIFEST: &str = "install-manifest.json";
const RECEIPT: &str = "install-receipt.json";

#[derive(Debug, Deserialize)]
struct PayloadFile {
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    version: String,
    target: String,
    accelerator: String,
    files: BTreeMap<String, PayloadFile>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema_version: u32,
    source: String,
    version: String,
    target: String,
    accelerator_requested: String,
    accelerator_actual: String,
    home: PathBuf,
    modify_path: bool,
    #[serde(default)]
    launcher: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Restart {
    home: PathBuf,
    listen: String,
    toolset: String,
    token_file: Option<PathBuf>,
}

impl Restart {
    fn from_config(config: &ServerConfig) -> Self {
        Self {
            home: config.home.clone(),
            listen: config.listen.to_string(),
            toolset: config.mcp_toolset.unwrap_or_default().to_string(),
            token_file: config.token_file.clone(),
        }
    }

    fn config(&self) -> Result<ServerConfig> {
        let mut config = ServerConfig::new(self.listen.parse()?, self.home.clone());
        config.mcp_toolset = Some(match self.toolset.as_str() {
            "agent" => zg_daemon::McpToolset::Agent,
            "full" => zg_daemon::McpToolset::Full,
            _ => return Err(io::Error::other("invalid saved daemon toolset").into()),
        });
        config.token_file.clone_from(&self.token_file);
        Ok(config)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    had_previous: bool,
    home: PathBuf,
    restart: Option<Restart>,
    #[serde(default)]
    launcher: Option<Launcher>,
}

fn binary(root: &Path) -> PathBuf {
    root.join("bin")
        .join(if cfg!(windows) { "zg.exe" } else { "zg" })
}

fn target() -> String {
    let os = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let abi = if cfg!(windows) {
        "-msvc"
    } else if cfg!(target_os = "linux") {
        "-gnu"
    } else {
        ""
    };
    format!("{os}-{arch}{abi}")
}

fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn regular_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty()
        || relative.contains('\\')
        || relative.contains(':')
        || !Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::other(format!("invalid payload path: {relative}")).into());
    }
    let mut path = root.to_owned();
    for part in Path::new(relative).components() {
        path.push(part);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(io::Error::other(format!("payload contains a link: {relative}")).into());
        }
    }
    if !path.is_file() {
        return Err(io::Error::other("payload is not a regular file").into());
    }
    Ok(path)
}

fn verify_bundle(root: &Path) -> Result<Manifest> {
    let manifest: Manifest = read_json(&regular_path(root, MANIFEST)?)?;
    if manifest.schema_version != 1
        || manifest.version != env!("CARGO_PKG_VERSION")
        || manifest.target != target()
        || !["cpu", "cuda"].contains(&manifest.accelerator.as_str())
        || !manifest.files.contains_key(if cfg!(windows) {
            "bin/zg.exe"
        } else {
            "bin/zg"
        })
    {
        return Err(io::Error::other("standalone bundle identity or platform mismatch").into());
    }
    for (relative, expected) in &manifest.files {
        let path = regular_path(root, relative)?;
        let mut file = File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        let mut actual = String::with_capacity(64);
        for byte in hash.finalize() {
            write!(actual, "{byte:02x}")?;
        }
        if file.metadata()?.len() != expected.bytes || actual != expected.sha256 {
            return Err(io::Error::other(format!(
                "standalone payload checksum mismatch: {relative}"
            ))
            .into());
        }
    }
    Ok(manifest)
}

fn copy_bundle(source: &Path, destination: &Path, manifest: &Manifest) -> Result<()> {
    fs::create_dir(destination)?;
    for relative in manifest
        .files
        .keys()
        .map(String::as_str)
        .chain(std::iter::once(MANIFEST))
    {
        let source = regular_path(source, relative)?;
        let output = destination.join(relative);
        fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| io::Error::other("missing payload parent"))?,
        )?;
        fs::copy(source, output)?;
    }
    Ok(())
}

fn remove_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(io::Error::other(format!(
                "refusing unexpected installation path: {}",
                path.display()
            ))
            .into());
        }
    }
    Ok(())
}

fn installation_lock(root: &Path) -> Result<File> {
    if fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::other("installation directory must not be a symlink").into());
    }
    fs::create_dir_all(root)?;
    let lock_path = root.join(".install.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(io::Error::other("invalid installation lock file").into());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.try_lock()
        .map_err(|_| io::Error::other("another standalone installation is running"))?;
    let marker = root.join(".zg-standalone");
    if marker.exists() {
        if fs::read_to_string(regular_path(root, ".zg-standalone")?)? != OWNER {
            return Err(io::Error::other("unknown installation owner").into());
        }
    } else {
        if fs::read_dir(root)?
            .any(|entry| entry.is_ok_and(|entry| entry.file_name() != ".install.lock"))
        {
            return Err(io::Error::other(
                "installation directory is not empty or belongs to another installer",
            )
            .into());
        }
        let mut owner = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)?;
        owner.write_all(OWNER.as_bytes())?;
        owner.sync_all()?;
    }
    Ok(lock)
}

fn check_binary(path: &Path, version: &str) -> Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != version {
        return Err(io::Error::other(format!(
            "installed executable failed its version check: {} ({}); {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    Ok(())
}

async fn rollback(root: &Path, pending: &Pending, daemon: &InstallationDaemon) -> Result<()> {
    daemon.stop().await?;
    let current = root.join("current");
    let previous = root.join("previous");
    if previous.exists() {
        remove_directory(&current)?;
        fs::rename(previous, &current)?;
    } else if !pending.had_previous {
        remove_directory(&current)?;
    }
    if let Some(launcher) = &pending.launcher {
        launcher.restore()?;
    }
    let restart = pending.restart.as_ref().map(Restart::config).transpose()?;
    daemon.restart(&binary(&current), restart.as_ref()).await?;
    fs::remove_file(root.join("pending.json"))?;
    Ok(())
}

async fn recover(root: &Path, token_file: Option<&Path>) -> Result<()> {
    if !root.join("pending.json").exists() {
        return Ok(());
    }
    let pending: Pending = read_json(&root.join("pending.json"))?;
    let token_file = token_file.or_else(|| {
        pending
            .restart
            .as_ref()
            .and_then(|restart| restart.token_file.as_deref())
    });
    let daemon = InstallationDaemon::acquire(
        &pending.home,
        &[
            binary(&root.join("current")),
            binary(&root.join("previous")),
        ],
        token_file,
    )
    .await?;
    rollback(root, &pending, &daemon).await?;
    eprintln!("Recovered the previous installation after an interrupted update.");
    Ok(())
}

fn prepare_candidate(
    args: &StandaloneArgs,
    executable: &Path,
    next: &Path,
    home: &Path,
    launcher: Option<&Launcher>,
) -> Result<Option<Receipt>> {
    remove_directory(next)?;
    if args.remove {
        return Ok(None);
    }
    let source_root = executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("invalid staging layout"))?;
    let manifest = verify_bundle(source_root)?;
    if (args.accelerator == "cpu" && manifest.accelerator != "cpu")
        || (args.accelerator == "cuda" && manifest.accelerator != "cuda")
    {
        return Err(io::Error::other("requested accelerator does not match the bundle").into());
    }
    copy_bundle(source_root, next, &manifest)?;
    verify_bundle(next)?;
    let receipt = Receipt {
        schema_version: 1,
        source: "https://github.com/zvec-ai/zvec-grep/releases".to_owned(),
        version: manifest.version,
        target: manifest.target,
        accelerator_requested: args.accelerator.clone(),
        accelerator_actual: manifest.accelerator,
        home: home.to_owned(),
        modify_path: !args.no_modify_path,
        launcher: launcher.map(|launcher| launcher.path.clone()),
    };
    write_json(&next.join(RECEIPT), &receipt)?;
    fs::write(
        next.join("install-state.tsv"),
        format!(
            "accelerator\t{}\nmodify_path\t{}\n",
            receipt.accelerator_requested,
            u8::from(receipt.modify_path)
        ),
    )?;
    check_binary(&binary(next), &receipt.version)?;
    Ok(Some(receipt))
}

fn check_data_layout(root: &Path, home: &Path) -> Result<()> {
    // The managed bin subtree may share the data home; no other data subtree may
    // become an installation root or be included in a program replacement.
    if home.starts_with(root) || (root.starts_with(home) && root != home.join("bin")) {
        return Err(
            io::Error::other("installation and daemon data directories must be separate").into(),
        );
    }
    Ok(())
}

fn installation_home(args: &StandaloneArgs, root: &Path, old: Option<&Receipt>) -> Result<PathBuf> {
    let home = zg_daemon::resolve_home(
        args.home
            .clone()
            .or_else(|| old.map(|receipt| receipt.home.clone())),
    )?;
    let home = std::path::absolute(home)?;
    if home.starts_with(root) {
        return Err(
            io::Error::other("installation and daemon data directories must be separate").into(),
        );
    }
    fs::create_dir_all(&home)?;
    let home = home.canonicalize()?;
    check_data_layout(root, &home)?;
    if let Some(receipt) = old
        && (receipt.schema_version != 1
            || receipt.target != target()
            || receipt.home.canonicalize()? != home)
    {
        return Err(io::Error::other("installation receipt mismatch; preserve the existing platform and daemon home when upgrading").into());
    }
    Ok(home)
}

pub async fn execute(args: &StandaloneArgs) -> Result<()> {
    let root = std::path::absolute(&args.install_dir)?;
    let _lock = installation_lock(&root)?;
    let root = root.canonicalize()?;
    recover(&root, args.token_file.as_deref()).await?;
    let current = root.join("current");
    let previous = root.join("previous");
    let next = root.join("next");
    let old: Option<Receipt> = if current.exists() {
        Some(read_json(&regular_path(
            &root,
            &format!("current/{RECEIPT}"),
        )?)?)
    } else {
        None
    };
    let home = installation_home(args, &root, old.as_ref())?;
    let launcher = args
        .launcher
        .as_ref()
        .or_else(|| old.as_ref().and_then(|old| old.launcher.as_ref()))
        .map(|path| Launcher::prepare(path, &root, args.remove))
        .transpose()?;
    if let Some(previous_launcher) = old.as_ref().and_then(|old| old.launcher.as_ref())
        && launcher
            .as_ref()
            .is_some_and(|launcher| &launcher.path != previous_launcher)
    {
        return Err(io::Error::other("preserve the existing command link when upgrading").into());
    }
    if args.remove && old.is_none() {
        return Err(io::Error::other("no standalone installation to remove").into());
    }
    let executable = std::env::current_exe()?.canonicalize()?;
    if executable.starts_with(&current)
        || executable.starts_with(&previous)
        || executable.starts_with(&next)
    {
        return Err(io::Error::other(
            "run install.sh or install.ps1; activation must run from a separate staging bundle",
        )
        .into());
    }
    let receipt = prepare_candidate(args, &executable, &next, &home, launcher.as_ref())?;
    // The prior rollback copy is removed before the journal is created, so its
    // presence unambiguously means that the current transaction moved the old bundle.
    remove_directory(&previous)?;
    let daemon =
        InstallationDaemon::acquire(&home, &[binary(&current)], args.token_file.as_deref()).await?;
    let pending = Pending {
        had_previous: old.is_some(),
        home,
        restart: daemon.config().map(Restart::from_config),
        launcher,
    };
    let journal = root.join(".pending-new.json");
    if journal.exists() {
        fs::remove_file(&journal)?;
    }
    write_json(&journal, &pending)?;
    fs::rename(journal, root.join("pending.json"))?;
    let result: Result<()> = async {
        daemon.stop().await?;
        if old.is_some() {
            fs::rename(&current, &previous)?;
        }
        if let Some(receipt) = &receipt {
            fs::rename(&next, &current)?;
            check_binary(&binary(&current), &receipt.version)?;
            if let Some(launcher) = &pending.launcher {
                launcher.install()?;
                check_binary(&launcher.path, &receipt.version)?;
            }
            daemon.restart(&binary(&current), daemon.config()).await?;
        } else if let Some(launcher) = &pending.launcher {
            launcher.remove()?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        if let Err(recovery) = rollback(&root, &pending, &daemon).await {
            return Err(io::Error::other(format!("installation failed: {error}; recovery failed: {recovery}; rerun the installer to recover")).into());
        }
        return Err(error);
    }
    fs::remove_file(root.join("pending.json"))?;
    drop(daemon);
    complete_installation(args, receipt.as_ref(), &current, &previous)
}

fn complete_installation(
    args: &StandaloneArgs,
    receipt: Option<&Receipt>,
    current: &Path,
    previous: &Path,
) -> Result<()> {
    if let Some(receipt) = receipt {
        println!(
            "Installed zvec-grep {} ({}, {}) at {}",
            receipt.version,
            receipt.target,
            receipt.accelerator_actual,
            binary(current).display()
        );
        if receipt.accelerator_actual == "cuda" {
            println!(
                "CUDA plugins installed. Compatible NVIDIA drivers and CUDA/cuBLAS/cuDNN host libraries are still required."
            );
        }
        if !args.skip_agent_upgrade {
            let launcher = receipt.launcher.clone().unwrap_or_else(|| binary(current));
            let status = Command::new(&launcher)
                .args(["--install", "--upgrade-installed", "--launcher"])
                .arg(&launcher)
                .stdin(Stdio::null())
                .status();
            if !status.is_ok_and(|status| status.success()) {
                eprintln!(
                    "Existing integration migration needs attention; rerun zg --install --upgrade-installed --launcher with the installed absolute executable path."
                );
            }
        }
        println!(
            "Reconnect existing MCP clients to use the new version. Re-run the installer to upgrade or select an older version."
        );
    } else {
        remove_directory(previous)?;
        println!("Removed standalone binaries. Configuration, models and indexes were preserved.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_managed_bin_subtree_may_share_the_data_home() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let home = temporary.path().join("data");
        let separate = temporary.path().join("program");
        assert!(check_data_layout(&home.join("bin"), &home).is_ok());
        assert!(check_data_layout(&separate, &home).is_ok());
        for root in [&home, &home.join("models"), &home.join("daemon")] {
            assert!(check_data_layout(root, &home).is_err());
        }
        assert!(check_data_layout(&separate, &separate.join("data")).is_err());
    }

    #[test]
    fn installation_ownership_and_lock_are_enforced() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("安装 with spaces");
        let lock = installation_lock(&root).expect("first lock");
        assert!(installation_lock(&root).is_err());
        drop(lock);
        assert!(installation_lock(&root).is_ok());
        fs::write(root.join(".zg-standalone"), "another installer").expect("owner");
        assert!(installation_lock(&root).is_err());
    }

    #[test]
    fn unknown_directories_and_payload_escapes_are_rejected() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        fs::write(temporary.path().join("user-file"), "preserve me").expect("fixture");
        assert!(installation_lock(temporary.path()).is_err());
        for path in ["../user-file", "/user-file", "C:/user-file", "a\\b"] {
            assert!(regular_path(temporary.path(), path).is_err());
        }
        assert_eq!(
            fs::read_to_string(temporary.path().join("user-file")).expect("user data"),
            "preserve me"
        );
    }
}
