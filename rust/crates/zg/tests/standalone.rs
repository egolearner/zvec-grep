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
        for name in [
            "CODEX_HOME",
            "GROK_HOME",
            "CLAUDE_CONFIG_DIR",
            "QWEN_HOME",
            "QODER_CONFIG_DIR",
            "QODER_IDE_MCP_PATH",
            "COPILOT_HOME",
            "CURSOR_CONFIG_DIR",
            "OPENCODE_CONFIG",
            "PI_CODING_AGENT_DIR",
            "VSCODE_PORTABLE",
            "ZVEC_GREP_INSTALL_SKIP_SERVER",
        ] {
            command.env_remove(name);
        }
        command
            .env("ZVEC_GREP_HOME", &self.home)
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join(".config"))
            .env("APPDATA", self.root.path().join("AppData/Roaming"))
            .env("VSCODE_USER_DIR", self.root.path().join("vscode/User"))
            .env_remove("ZVEC_GREP_SERVER_TOKEN")
            .env_remove("ZVEC_GREP_SERVER_TOKEN_FILE");
        if let Some(token) = &self.token {
            command.env("ZVEC_GREP_SERVER_TOKEN", token);
        }
        command
    }

    fn install_command(&self) -> Command {
        let mut command = self.install_with_agents_command();
        command.arg("--skip-agent-upgrade");
        command
    }

    fn install_with_agents_command(&self) -> Command {
        let mut command = self.command(&executable_path(&self.bundle));
        command
            .args(["--standalone", "--install-dir"])
            .arg(&self.install);
        #[cfg(unix)]
        command.arg("--launcher").arg(self.launcher());
        command
            .args(["--accelerator", "cpu", "--no-modify-path"])
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

    fn normalized_launcher(&self) -> PathBuf {
        let launcher = self.launcher();
        // Resolve directory aliases without following the stable command symlink itself.
        launcher
            .parent()
            .expect("launcher directory")
            .canonicalize()
            .expect("canonical launcher directory")
            .join(launcher.file_name().expect("launcher filename"))
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

    fn agent_config(&self) -> PathBuf {
        self.root.path().join(".cursor/mcp.json")
    }

    fn write_agent_config(&self) -> Value {
        let config = json!({"mcpServers": {
            "zvec_grep": {
                "command": "node", "args": [self.root.path().join("node_modules/@zvec/zvec-grep/dist/cli/index.js"), "server", "--stdio"],
                "env": {"ZVEC_GREP_HOME": self.home, "CUSTOM": "preserved"}, "timeout": 913
            },
            "native": {"command": executable_path(&self.root.path().join("old install")), "args": ["--server", "--stdio", "--home", self.home]},
            "http": {"url": "http://127.0.0.1:9123/mcp", "headers": {"Authorization": "Bearer keep"}},
            "unrelated": {"command": "other", "args": []}
        }});
        fs::create_dir_all(self.agent_config().parent().expect("config directory"))
            .expect("config directory");
        fs::write(
            self.agent_config(),
            serde_json::to_vec_pretty(&config).expect("JSON"),
        )
        .expect("agent config");
        config
    }

    fn agent_backups(&self) -> Vec<PathBuf> {
        fs::read_dir(self.agent_config().parent().expect("config directory"))
            .expect("config directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.to_string_lossy().contains(".zg-upgrade-"))
            .collect()
    }

    fn assert_agent_migrated(&self, mut original: Value) {
        let launcher = self.normalized_launcher();
        original["mcpServers"]["zvec_grep"]["command"] = json!(launcher);
        original["mcpServers"]["native"]["command"] = json!(launcher);
        original["mcpServers"]["zvec_grep"]["args"] = json!(["--server", "--stdio"]);
        let updated: Value =
            serde_json::from_slice(&fs::read(self.agent_config()).expect("config"))
                .expect("updated JSON");
        assert_eq!(updated, original);
        assert_eq!(self.agent_backups().len(), 1);
    }

    fn assert_agent_unchanged(&self, original: &[u8]) {
        assert_eq!(fs::read(self.agent_config()).expect("config"), original);
        assert!(self.agent_backups().is_empty());
    }

    fn assert_external_preview(&self, output: Output, executable: &Path) {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        success(output);
        for message in [
            "External daemon left running",
            "preview-only",
            "Would upgrade: 2",
            "Manual daemon switch required",
        ] {
            assert!(stdout.contains(message), "{stdout}");
        }
        let stop = stdout
            .lines()
            .find(|line| line.contains("3. Stop the external daemon"))
            .expect("manual stop command");
        assert!(
            stop.contains(self.normalized_launcher().to_string_lossy().as_ref()),
            "{stop}"
        );
        assert!(
            stdout.contains(
                executable
                    .canonicalize()
                    .expect("executable")
                    .to_string_lossy()
                    .as_ref()
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                self.home
                    .canonicalize()
                    .expect("canonical home")
                    .to_string_lossy()
                    .as_ref()
            ),
            "{stdout}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .command(Path::new(env!("CARGO_BIN_EXE_zg")))
            .args(["--server", "off", "--home"])
            .arg(&self.home)
            .output();
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
    let original_config = fixture.write_agent_config();
    fixture.write_manifest("second");
    success(
        fixture
            .install_with_agents_command()
            .output()
            .expect("upgrade with agents"),
    );
    fixture.assert_agent_migrated(original_config);
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
fn external_daemon_allows_install_and_recovery_but_only_previews_agent_migration() {
    let mut fixture = Fixture::new();
    fixture.token = Some("external-daemon-regression-token-at-least-32-characters".to_owned());
    let original_config = fixture.write_agent_config();
    let original_bytes = fs::read(fixture.agent_config()).expect("original config");
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
    let instance = fixture.instance_token();
    let install_without_credentials = || {
        fixture
            .install_with_agents_command()
            .env_remove("ZVEC_GREP_SERVER_TOKEN")
            .output()
            .expect("install alongside external daemon")
    };
    fixture.assert_external_preview(install_without_credentials(), executable);
    assert!(fixture.installed().exists());
    assert_eq!(fixture.instance_token(), instance);
    fixture.assert_agent_unchanged(&original_bytes);

    // Recovery without an owned service must also leave the external daemon untouched.
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("interrupted move");
    fs::write(
        fixture.install.join("pending.json"),
        serde_json::to_vec(&json!({
            "hadPrevious": true, "home": fixture.home, "restart": null
        }))
        .expect("journal"),
    )
    .expect("write journal");
    fixture.write_manifest("second");
    success(install_without_credentials());
    assert!(!fixture.install.join("pending.json").exists());
    assert_eq!(fixture.instance_token(), instance);
    fixture.assert_agent_unchanged(&original_bytes);
    assert_eq!(
        fs::read_to_string(fixture.install.join("current/marker")).expect("current"),
        "second"
    );

    success(
        fixture
            .command(&executable_path(&fixture.bundle))
            .args(["--standalone", "--remove", "--install-dir"])
            .arg(&fixture.install)
            .env_remove("ZVEC_GREP_SERVER_TOKEN")
            .output()
            .expect("uninstall alongside external daemon"),
    );
    assert!(!fixture.installed().exists());
    assert_eq!(fixture.instance_token(), instance);
    fixture.assert_agent_unchanged(&original_bytes);
    success(install_without_credentials());
    success(
        fixture
            .command(&fixture.launcher())
            .args(["--server", "off", "--home"])
            .arg(&fixture.home)
            .output()
            .expect("stop external installation"),
    );
    success(install_without_credentials());
    fixture.assert_agent_migrated(original_config);
    assert!(std::net::TcpStream::connect(address).is_err());
}

#[test]
fn recovery_cannot_replace_an_external_daemon_when_owned_restart_is_pending() {
    let fixture = Fixture::new();
    success(fixture.install());
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("interrupted move");
    let socket = TcpListener::bind("127.0.0.1:0").expect("port");
    let address = socket.local_addr().expect("address").to_string();
    drop(socket);
    fs::write(fixture.install.join("pending.json"), serde_json::to_vec(&json!({
        "hadPrevious": true, "home": fixture.home,
        "restart": { "home": fixture.home, "listen": address, "toolset": "agent", "tokenFile": null, "authenticationRequired": false }
    })).expect("journal")).expect("write journal");
    success(
        fixture
            .command(Path::new(env!("CARGO_BIN_EXE_zg")))
            .args(["--server", "on", "--listen", &address, "--home"])
            .arg(&fixture.home)
            .output()
            .expect("start external daemon"),
    );
    let instance = fixture.instance_token();
    let output = fixture.install();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another executable"));
    assert_eq!(fixture.instance_token(), instance);
    assert!(fixture.install.join("pending.json").exists());
    assert!(fixture.install.join("previous").exists());
    assert!(!fixture.install.join("current").exists());
}

#[test]
fn daemon_running_from_previous_bundle_prevents_deleting_its_executable() {
    let fixture = Fixture::new();
    success(fixture.install());
    fs::rename(
        fixture.install.join("current"),
        fixture.install.join("previous"),
    )
    .expect("move previous bundle");
    let socket = TcpListener::bind("127.0.0.1:0").expect("port");
    let address = socket.local_addr().expect("address").to_string();
    drop(socket);
    let executable = executable_path(&fixture.install.join("previous"));
    success(
        fixture
            .command(&executable)
            .args(["--server", "on", "--listen", &address, "--home"])
            .arg(&fixture.home)
            .output()
            .expect("start misplaced daemon"),
    );
    let instance = fixture.instance_token();
    let output = fixture.install();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another executable"));
    assert_eq!(fixture.instance_token(), instance);
    assert!(executable.exists());
    assert!(!fixture.install.join("pending.json").exists());
    assert!(!fixture.install.join("next").exists());
}

#[test]
fn agent_migration_failure_does_not_roll_back_a_successful_installation() {
    let fixture = Fixture::new();
    fixture.write_agent_config();
    fs::write(fixture.agent_config(), "{ invalid json").expect("broken config");
    let output = fixture
        .install_with_agents_command()
        .output()
        .expect("install");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    success(output);
    assert!(stderr.contains("migration needs attention"), "{stderr}");
    assert!(fixture.installed().exists());
    assert!(!fixture.install.join("pending.json").exists());
    assert_eq!(
        fs::read_to_string(fixture.agent_config()).expect("config"),
        "{ invalid json"
    );
    assert!(fixture.agent_backups().is_empty());
}
