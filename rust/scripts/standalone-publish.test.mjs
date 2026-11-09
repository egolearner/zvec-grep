import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { loadReleaseManifest, sha256File } from "./npm-package-core.mjs";
import { publishStandaloneRelease, verifyStandaloneRelease } from "./standalone-publish.mjs";

async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), "zg-standalone-publish-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const release = { schemaVersion: 1, version: "1.2.3", tag: "rust-v1.2.3", commit: "a".repeat(40), partial: false, assets: [] };
  for (const entry of loadReleaseManifest().platforms) {
    for (const accelerator of entry.components?.length ? ["cpu", "cuda"] : ["cpu"]) {
      const file = `zvec-grep-${release.version}-${entry.target}-${accelerator}.${entry.os === "win32" ? "zip" : "tar.gz"}`;
      await writeFile(join(directory, file), `payload for ${file}`);
      release.assets.push({ target: entry.target, accelerator, file, bytes: (await stat(join(directory, file))).size,
        sha256: await sha256File(join(directory, file)) });
    }
  }
  await writeFile(join(directory, "manifest.json"), JSON.stringify(release));
  await writeFile(join(directory, "manifest.tsv"), `schema\t1\nversion\t${release.version}\ntag\t${release.tag}\n` +
    release.assets.map((asset) => `${asset.target}\t${asset.accelerator}\t${asset.file}\t${asset.sha256}\t${asset.bytes}\n`).join(""));
  for (const file of ["install.sh", "install.ps1"]) await writeFile(join(directory, file), "installer fixture");
  const files = [...release.assets.map((asset) => asset.file), "manifest.json", "manifest.tsv", "install.sh", "install.ps1"];
  await writeFile(join(directory, "SHA256SUMS"), (await Promise.all(files.map(async (file) => `${await sha256File(join(directory, file))}  ${file}\n`))).join(""));
  files.push("SHA256SUMS");
  const assets = await Promise.all(files.map(async (file) => ({ name: file, size: (await stat(join(directory, file))).size,
    digest: `sha256:${await sha256File(join(directory, file))}`, state: "uploaded" })));
  return { directory, release, assets, repository: "zvec-ai/zvec-grep", ref: `refs/tags/${release.tag}`, commit: release.commit };
}

test("publication waits for a complete server-verified draft before promoting Latest", async (t) => {
  const data = await fixture(t);
  const calls = [];
  let created = false;
  const gh = (args) => {
    calls.push(args);
    if (args[0] === "api") {
      if (!created) throw new Error("HTTP 404: Not Found");
      return JSON.stringify({ draft: true, assets: data.assets });
    }
    if (args[1] === "create") { created = true; assert.ok(args.includes("--draft")); }
    return "";
  };
  await publishStandaloneRelease({ ...data, gh });
  assert.deepEqual(calls.map((args) => args[0] === "api" ? "verify" : args[1]), ["verify", "create", "upload", "verify", "edit"]);
  assert.ok(calls.at(-1).includes("--latest"));
});

test("incomplete uploads and unavailable API cannot publish or create an unverified release", async (t) => {
  const data = await fixture(t);
  const calls = [];
  await assert.rejects(publishStandaloneRelease({ ...data, gh: (args) => {
    calls.push(args);
    return JSON.stringify({ draft: true, assets: data.assets.slice(1) });
  } }), /digest mismatch/);
  assert.equal(calls.some((args) => args[1] === "edit"), false);
  calls.length = 0;
  await assert.rejects(publishStandaloneRelease({ ...data, gh: (args) => { calls.push(args); throw new Error("HTTP 403: denied"); } }), /403/);
  assert.equal(calls.length, 1);
});

test("published releases are immutable and identical retries never move Latest", async (t) => {
  const data = await fixture(t);
  const calls = [];
  const gh = (args) => { calls.push(args); return JSON.stringify({ draft: false, assets: data.assets }); };
  await publishStandaloneRelease({ ...data, gh });
  assert.equal(calls.length, 1);
  data.assets[0].digest = `sha256:${"0".repeat(64)}`;
  await assert.rejects(publishStandaloneRelease({ ...data, gh }), /digest mismatch/);
  assert.equal(calls.length, 2);
});

test("partial, corrupt, mismatched manifests and wrong source identities never reach GitHub", async (t) => {
  const data = await fixture(t);
  const gh = () => assert.fail("invalid input must not reach GitHub");
  await assert.rejects(publishStandaloneRelease({ ...data, repository: "fork/project", gh }), /official repository/);
  await assert.rejects(publishStandaloneRelease({ ...data, commit: "b".repeat(40), gh }), /exact verified/);
  const manifest = join(data.directory, "manifest.json");
  const original = await readFile(manifest);
  await writeFile(manifest, JSON.stringify({ ...data.release, partial: true }));
  await assert.rejects(verifyStandaloneRelease(data.directory), /complete tagged stable/);
  await writeFile(manifest, original);
  const table = join(data.directory, "manifest.tsv");
  const originalTable = await readFile(table);
  await writeFile(table, "wrong table");
  await assert.rejects(verifyStandaloneRelease(data.directory), /manifests disagree/);
  await writeFile(table, originalTable);
  await writeFile(join(data.directory, data.release.assets[0].file), "tampered payload");
  await assert.rejects(publishStandaloneRelease({ ...data, gh }), /payload mismatch/);
});
