use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn hash(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in Sha256::digest(bytes) {
        write!(output, "{byte:02x}").expect("format digest");
    }
    output
}

fn copy_libraries(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("library directory");
    for entry in fs::read_dir(source).expect("build output") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file()
            && (name.contains(".so")
                || entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("dylib") || extension.eq_ignore_ascii_case("dll")
                }))
        {
            fs::copy(entry.path(), destination.join(name)).expect("runtime library");
        }
    }
}

struct Fixture {
    root: TempDir,
    bundle: PathBuf,
    install: PathBuf,
    home: PathBuf,
    token: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture");
        let bundle = root.path().join("bundle");
        let home = root.path().join("用户 data");
        let install = home.join("bin");
        fs::create_dir_all(bundle.join("bin")).expect("bin");
        let executable = Path::new(env!("CARGO_BIN_EXE_zg"));
        fs::copy(executable, executable_path(&bundle)).expect("binary");
        let profile = executable.parent().expect("profile");
        copy_libraries(profile, &bundle.join("lib"));
        if profile.join("lib").is_dir() {
            copy_libraries(&profile.join("lib"), &bundle.join("lib"));
        }
        let fixture = Self {
            root,
            bundle,
            install,
            home,
            token: None,
        };
        fixture.write_manifest("first");
        fixture
    }

    fn write_manifest(&self, marker: &str) {
        fs::write(self.bundle.join("marker"), marker).expect("marker");
        let mut files = BTreeMap::new();
        for directory in [self.bundle.join("bin"), self.bundle.join("lib")] {
            for entry in fs::read_dir(directory).expect("files") {
                let path = entry.expect("file").path();
                let content = fs::read(&path).expect("payload");
                files.insert(
                    path.strip_prefix(&self.bundle)
                        .expect("relative")
                        .to_string_lossy()
                        .replace('\\', "/"),
                    json!({ "bytes": content.len(), "sha256": hash(&content) }),
                );
            }
        }
        files.insert(
            "marker".to_owned(),
            json!({ "bytes": marker.len(), "sha256": hash(marker.as_bytes()) }),
        );
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
        fs::write(self.bundle.join("install-manifest.json"), serde_json::to_vec(&json!({
            "schemaVersion": 1, "version": env!("CARGO_PKG_VERSION"), "target": format!("{os}-{arch}{abi}"),
            "accelerator": "cpu", "files": files
        })).expect("manifest")).expect("write manifest");
    }

    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .env("ZVEC_GREP_HOME", &self.home)
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env_remove("ZVEC_GREP_SERVER_TOKEN")
            .env_remove("ZVEC_GREP_SERVER_TOKEN_FILE");
        if let Some(token) = &self.token {
            command.env("ZVEC_GREP_SERVER_TOKEN", token);
        }
        command
    }

    fn install_command(&self) -> Command {
        let mut command = self.command(&executable_path(&self.bundle));
        command
            .args(["--standalone", "--install-dir"])
            .arg(&self.install);
        #[cfg(unix)]
        command.arg("--launcher").arg(self.launcher());
        command
            .args([
                "--accelerator",
                "cpu",
                "--no-modify-path",
                "--skip-agent-upgrade",
            ])
            .arg("--home")
            .arg(&self.home);
        command
    }

    fn install(&self) -> Output {
        self.install_command().output().expect("install")
    }

    fn installed(&self) -> PathBuf {
        executable_path(&self.install.join("current"))
    }

    fn launcher(&self) -> PathBuf {
        if cfg!(unix) {
            self.root.path().join(".local/bin/zg")
        } else {
            self.installed()
        }
    }

    fn instance(&self) -> Value {
        serde_json::from_slice(&fs::read(self.home.join("daemon/instance.lock")).expect("instance"))
            .expect("record")
    }

    fn pid(&self) -> u64 {
        self.instance()["pid"].as_u64().expect("PID")
    }

    fn instance_token(&self) -> String {
        self.instance()["instanceToken"]
            .as_str()
            .expect("instance token")
            .to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.installed().exists() {
            let _ = self
                .command(&self.installed())
                .args(["--server", "off", "--home"])
                .arg(&self.home)
                .output();
        }
    }
}

fn executable_path(root: &Path) -> PathBuf {
    root.join("bin")
        .join(if cfg!(windows) { "zg.exe" } else { "zg" })
}

#[track_caller]
fn success(output: Output) {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    assert!(
        status.success(),
        "status: {status}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
}

#[cfg(unix)]
fn conflicting_launcher_keeps_the_daemon(fixture: &Fixture, pid: u64) {
    fs::remove_file(fixture.launcher()).expect("replace command link");
    fs::write(fixture.launcher(), "foreign program").expect("foreign entry");
    let output = fixture.install();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another installation"));
    assert_eq!(fixture.pid(), pid);
    assert_eq!(
        fs::read_to_string(fixture.launcher()).expect("preserved entry"),
        "foreign program"
    );
    fs::remove_file(fixture.launcher()).expect("remove fixture");
    std::os::unix::fs::symlink(
        fixture.installed().canonicalize().expect("target"),
        fixture.launcher(),
    )
    .expect("restore command link");
}

#[test]
fn standalone_upgrade_restarts_owned_daemon_preserves_data_and_recovers_interruption() {
    let fixture = Fixture::new();
    success(fixture.install());
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(fixture.launcher()).expect("command link"),
        fixture
            .installed()
            .canonicalize()
            .expect("installed binary")
    );
    fs::write(fixture.home.join("user-data"), "keep").expect("data");
    let socket = TcpListener::bind("127.0.0.1:0").expect("port");
    let address = socket.local_addr().expect("address").to_string();
    drop(socket);
    success(
        fixture
            .command(&fixture.launcher())
            .args(["--server", "on", "--listen", &address, "--home"])
            .arg(&fixture.home)
            .output()
            .expect("start"),
    );
    let first_instance = fixture.instance_token();
    fixture.write_manifest("second");
    success(fixture.install());
    // Windows may reuse a PID immediately; the instance token identifies each daemon lifetime.
    assert_ne!(fixture.instance_token(), first_instance);
    assert_eq!(
        fs::read_to_string(fixture.install.join("previous/marker")).expect("previous"),
        "first"
    );
    assert_eq!(
        fs::read_to_string(fixture.install.join("current/marker")).expect("current"),
        "second"
    );
    let second_pid = fixture.pid();
    #[cfg(unix)]
    conflicting_launcher_keeps_the_daemon(&fixture, second_pid);
    fs::write(fixture.bundle.join("marker"), "corrupt").expect("tamper");
    let output = fixture.install();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"));
    assert_eq!(fixture.pid(), second_pid);
    fixture.write_manifest("third");
    success(
        fixture
            .command(&fixture.installed())
            .args(["--server", "off", "--home"])
            .arg(&fixture.home)
            .output()
            .expect("stop"),
    );
    fs::remove_dir_all(fixture.install.join("previous")).expect("remove old backup");
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("interrupted move");
    fs::write(fixture.install.join("pending.json"), serde_json::to_vec(&json!({
        "hadPrevious": true, "home": fixture.home,
        "restart": { "home": fixture.home, "listen": address, "toolset": "agent", "tokenFile": null, "authenticationRequired": false }
    })).expect("journal")).expect("write journal");
    success(fixture.install());
    assert!(!fixture.install.join("pending.json").exists());
    assert_eq!(
        fs::read_to_string(fixture.install.join("previous/marker")).expect("recovered backup"),
        "second"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("user-data")).expect("data"),
        "keep"
    );
    assert!(fixture.pid() > 0);
    success(
        fixture
            .command(&executable_path(&fixture.bundle))
            .args(["--standalone", "--install-dir"])
            .arg(&fixture.install)
            .arg("--remove")
            .output()
            .expect("uninstall"),
    );
    assert!(!fixture.installed().exists());
    #[cfg(unix)]
    assert!(fs::symlink_metadata(fixture.launcher()).is_err());
    assert_eq!(
        fs::read_to_string(fixture.home.join("user-data")).expect("preserved data"),
        "keep"
    );
}

#[test]
fn interrupted_authenticated_installation_requires_credentials_before_restarting() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;

    let mut fixture = Fixture::new();
    success(fixture.install());
    fixture.token = Some("standalone-regression-token-at-least-32-characters".to_owned());
    let listener = TcpListener::bind("127.0.0.1:0").expect("port");
    let address = listener.local_addr().expect("address").to_string();
    drop(listener);
    let anonymous_status = || {
        let mut stream = TcpStream::connect(&address).expect("daemon connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        write!(stream, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        response.lines().next().expect("status").to_owned()
    };
    success(
        fixture
            .command(&fixture.installed())
            .args(["--server", "on", "--listen", &address, "--home"])
            .arg(&fixture.home)
            .output()
            .expect("start"),
    );
    assert!(anonymous_status().contains("401"));
    // Missing credentials must fail before creating a journal or stopping the service.
    let denied = fixture
        .install_command()
        .env_remove("ZVEC_GREP_SERVER_TOKEN")
        .output()
        .expect("missing credentials");
    assert!(!denied.status.success());
    assert!(!fixture.install.join("pending.json").exists());
    assert!(anonymous_status().contains("401"));
    success(
        fixture
            .command(&fixture.installed())
            .args(["--server", "off", "--home"])
            .arg(&fixture.home)
            .output()
            .expect("stop"),
    );
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("interrupted move");
    fs::write(fixture.install.join("pending.json"), serde_json::to_vec(&json!({
        "hadPrevious": true, "home": fixture.home,
        "restart": { "home": fixture.home, "listen": address, "toolset": "agent", "tokenFile": null, "authenticationRequired": true }
    })).expect("journal")).expect("write journal");
    let denied = fixture
        .install_command()
        .env_remove("ZVEC_GREP_SERVER_TOKEN")
        .output()
        .expect("recovery without token");
    assert!(!denied.status.success());
    assert!(
        String::from_utf8_lossy(&denied.stderr)
            .contains("recovery requires the daemon authentication token")
    );
    assert!(TcpStream::connect(&address).is_err());
    assert!(fixture.install.join("pending.json").exists());
    assert!(fixture.install.join("previous").exists());
    success(fixture.install());
    assert!(anonymous_status().contains("401"));
    assert!(!fixture.install.join("pending.json").exists());
}

#[test]
fn standalone_uninstall_recovers_an_interrupted_directory_move() {
    let fixture = Fixture::new();
    success(fixture.install());
    fs::write(fixture.home.join("user-data"), "keep").expect("data");
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("move");
    fs::write(
        fixture.install.join("pending.json"),
        serde_json::to_vec(&json!({
            "hadPrevious": true, "home": fixture.home, "restart": null
        }))
        .expect("journal"),
    )
    .expect("write journal");
    success(
        fixture
            .command(&executable_path(&fixture.bundle))
            .args(["--standalone", "--remove", "--install-dir"])
            .arg(&fixture.install)
            .output()
            .expect("recover and remove"),
    );
    for name in ["current", "previous", "next", "pending.json"] {
        assert!(!fixture.install.join(name).exists(), "{name}");
    }
    #[cfg(unix)]
    assert!(fs::symlink_metadata(fixture.launcher()).is_err());
    assert_eq!(
        fs::read_to_string(fixture.home.join("user-data")).expect("preserved data"),
        "keep"
    );
}

#[cfg(unix)]
#[test]
fn standalone_uninstall_preserves_a_replaced_command_entry() {
    let fixture = Fixture::new();
    success(fixture.install());
    fs::remove_file(fixture.launcher()).expect("replace link");
    fs::write(fixture.launcher(), "unrelated executable").expect("replacement");
    success(
        fixture
            .command(&executable_path(&fixture.bundle))
            .args(["--standalone", "--install-dir"])
            .arg(&fixture.install)
            .arg("--remove")
            .output()
            .expect("uninstall"),
    );
    assert_eq!(
        fs::read_to_string(fixture.launcher()).expect("preserved command"),
        "unrelated executable"
    );
    assert!(!fixture.installed().exists());
}

#[test]
fn standalone_refuses_a_daemon_owned_by_another_installation() {
    let fixture = Fixture::new();
    let socket = TcpListener::bind("127.0.0.1:0").expect("port");
    let address = socket.local_addr().expect("address").to_string();
    drop(socket);
    let executable = Path::new(env!("CARGO_BIN_EXE_zg"));
    success(
        fixture
            .command(executable)
            .args(["--server", "on", "--listen", &address, "--home"])
            .arg(&fixture.home)
            .output()
            .expect("start external installation"),
    );
    let pid = fixture.pid();
    let output = fixture.install();
    let same_pid = fixture.pid();
    success(
        fixture
            .command(executable)
            .args(["--server", "off", "--home"])
            .arg(&fixture.home)
            .output()
            .expect("stop external installation"),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another executable"));
    assert_eq!(pid, same_pid);
    assert!(!fixture.installed().exists());
}
