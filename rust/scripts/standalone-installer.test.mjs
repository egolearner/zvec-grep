import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { workspaceDir } from "./npm-package-core.mjs";
import { installerEnvironment } from "./standalone-smoke.mjs";

const execute = promisify(execFile);
const shellInstaller = join(workspaceDir, "standalone/install.sh");
const powerShellInstaller = join(workspaceDir, "standalone/install.ps1");

async function temporary(t) {
  const root = await mkdtemp(join(tmpdir(), "zg-installer-regression-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

test("the Node-free smoke environment can extract gzip archives", { skip: process.platform === "win32" }, async (t) => {
  const root = await temporary(t);
  const env = await installerEnvironment(root);
  await writeFile(join(root, "payload"), "native payload");
  const archive = join(root, "payload.tar.gz");
  await execute("tar", ["-czf", archive, "-C", root, "payload"]);
  const output = join(root, "extracted");
  await mkdir(output);
  await execute("tar", ["-xzf", archive, "-C", output], { env });
  assert.equal(await readFile(join(output, "payload"), "utf8"), "native payload");
  await assert.rejects(execute("node", ["--version"], { env }), { code: "ENOENT" });
});

test("shell uninstall enters native recovery when only previous exists", { skip: process.platform === "win32" }, async (t) => {
  const root = await temporary(t);
  const install = join(root, "安装 with spaces/bin");
  const executable = join(install, "previous/bin/zg");
  await mkdir(dirname(executable), { recursive: true });
  await writeFile(executable, '#!/bin/sh\nprintf "%s\\n" "$@" > "$REVIEW_ARGUMENTS"\n');
  await chmod(executable, 0o755);
  await writeFile(join(install, "previous/install-receipt.json"), "{}");
  await writeFile(join(install, "previous/install-state.tsv"), "repository\tegolearner/zvec-grep\n");
  await writeFile(join(install, "pending.json"), "{}");
  const output = join(root, "arguments");
  const env = { ...process.env, HOME: root, ZG_INSTALL_DIR: install, ZG_INSTALL_TMPDIR: root,
    ZG_INSTALL_REPOSITORY: "", ZG_INSTALL_ACCELERATOR: "cpu", REVIEW_ARGUMENTS: output };
  await execute("sh", [shellInstaller, "--uninstall"], { env });
  const args = (await readFile(output, "utf8")).split("\n");
  assert.ok(args.includes("--remove"));
  assert.equal(args[args.indexOf("--repository") + 1], "egolearner/zvec-grep");
  for (const repository of ["https://example.com/repo", "owner/../repo", "../repo", "owner/repo?redirect=1"]) {
    await assert.rejects(execute("sh", [shellInstaller, "--uninstall"], { env: { ...env, ZG_INSTALL_REPOSITORY: repository } }), /GitHub owner\/name/);
  }
});

test("GPU detection terminates a probe that ignores SIGTERM", { skip: process.platform === "win32", timeout: 12000 }, async (t) => {
  const root = await temporary(t);
  const source = await readFile(shellInstaller, "utf8");
  const probe = source.match(/^has_nvidia\(\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(probe);
  const fake = join(root, "nvidia-smi");
  await writeFile(fake, '#!/bin/sh\ntrap "" TERM\nexec sleep 30\n');
  await chmod(fake, 0o755);
  const child = spawn("sh", ["-c", `${probe}\nif has_nvidia; then exit 2; fi\nprintf completed`], {
    detached: true, env: { ...process.env, PATH: `${root}:${process.env.PATH}`, staging: root }, stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.resume();
  let timer;
  try {
    const code = await Promise.race([
      new Promise((accept, reject) => { child.once("error", reject); child.once("close", accept); }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("GPU probe exceeded its hard deadline")), 8000); }),
    ]);
    assert.equal(code, 0);
    assert.equal(stdout, "completed");
  } finally {
    clearTimeout(timer);
    try { process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
  }
});

test("Windows PATH updates respect opt-out and command ownership", { skip: process.platform !== "win32" }, async (t) => {
  const root = await temporary(t);
  const source = await readFile(powerShellInstaller, "utf8");
  const update = source.match(/^function Get-UpdatedUserPath\([^\n]+\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(update);
  const script = join(root, "path-test.ps1");
  await writeFile(script, `${update}\n$ErrorActionPreference = 'Stop'
$pathDir = 'C:\\Program Files\\zg\\current\\bin'
$original = "C:\\user;$pathDir;C:\\tools"
foreach ($remove in @($true, $false)) {
    if ($null -ne (Get-UpdatedUserPath $original $pathDir $remove $false $true)) { throw 'Opt-out must leave PATH unchanged' }
}
if ($null -ne (Get-UpdatedUserPath $original $pathDir $true $true $false)) { throw 'Uninstall must preserve a user-owned entry' }
if ((Get-UpdatedUserPath $original $pathDir $true $true $true) -ne 'C:\\user;C:\\tools') { throw 'Remove only the managed entry' }
$installed = Get-UpdatedUserPath 'C:\\user' $pathDir $false $true $false
if ((Get-UpdatedUserPath $installed $pathDir $false $true $true) -ne $installed) { throw 'Install must be idempotent' }
`);
  await execute("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script]);
});

test("shell staged version checks reject mismatches and have a hard deadline", { skip: process.platform === "win32", timeout: 16000 }, async (t) => {
  const root = await temporary(t);
  const source = await readFile(shellInstaller, "utf8");
  const check = source.match(/^check_version\(\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(check);
  const fake = join(root, "zg");
  const env = { ...process.env, staging: root, REVIEW_EXECUTABLE: fake };
  await writeFile(fake, '#!/bin/sh\nprintf 1.2.3\n');
  await chmod(fake, 0o755);
  await execute("sh", ["-c", `${check}\ncheck_version "$REVIEW_EXECUTABLE" 1.2.3`], { env });
  await assert.rejects(execute("sh", ["-c", `${check}\ncheck_version "$REVIEW_EXECUTABLE" 1.2.4`], { env }));
  await writeFile(fake, '#!/bin/sh\ntrap "" TERM\nexec sleep 30\n');
  await assert.rejects(execute("sh", ["-c", `${check}\ncheck_version "$REVIEW_EXECUTABLE" 1.2.3`], {
    env, timeout: 13000, killSignal: "SIGKILL",
  }), (error) => {
    assert.equal(error.killed, false, "the installer must terminate the probe before the test deadline");
    assert.equal(error.code, 1);
    return true;
  });
});

test("PowerShell staged version checks accept only the expected executable version", { skip: process.platform !== "win32" }, async (t) => {
  const root = await temporary(t);
  const source = await readFile(powerShellInstaller, "utf8");
  const check = source.match(/^function Test-StagedVersion\([^\n]+\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(check);
  const script = join(root, "version-test.ps1");
  await writeFile(script, `${check}\n$ErrorActionPreference = 'Stop'\nTest-StagedVersion $env:REVIEW_EXECUTABLE $env:REVIEW_VERSION\n`);
  const env = { ...process.env, REVIEW_EXECUTABLE: process.execPath, REVIEW_VERSION: process.version };
  await execute("powershell.exe", ["-NoProfile", "-File", script], { env });
  await assert.rejects(execute("powershell.exe", ["-NoProfile", "-File", script], {
    env: { ...env, REVIEW_VERSION: "wrong" },
  }), /version mismatch/);
});

test("PowerShell uninstall reaches the staged executable when current is missing", { skip: process.platform !== "win32" }, async (t) => {
  const root = await temporary(t);
  const install = join(root, "program");
  await mkdir(join(install, "previous/bin"), { recursive: true });
  // Node rejects the activation flag, proving that the previous bundle was staged and invoked.
  await copyFile(process.execPath, join(install, "previous/bin/zg.exe"));
  await writeFile(join(install, "previous/install-receipt.json"), "{}");
  await writeFile(join(install, "pending.json"), "{}");
  for (const accelerator of [[], ["-Accelerator", "cpu"]]) {
    await assert.rejects(execute("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", powerShellInstaller,
      "-InstallDir", install, ...accelerator, "-NoModifyPath", "-Uninstall"], {
      env: { ...process.env, ZG_INSTALL_TMPDIR: root, ZG_INSTALL_REPOSITORY: "zvec-ai/zvec-grep", ZG_INSTALL_ACCELERATOR: "" },
    }), (error) => {
      assert.match(error.stderr, /bad option: --standalone/);
      assert.doesNotMatch(error.stderr, /No standalone installation found/);
      return true;
    });
  }
});
