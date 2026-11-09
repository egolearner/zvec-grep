#!/usr/bin/env node

import { readFile, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { loadReleaseManifest, run, sha256File, workspaceDir } from "./npm-package-core.mjs";

const metadata = ["manifest.json", "manifest.tsv", "SHA256SUMS", "install.sh", "install.ps1"];

export async function verifyStandaloneRelease(directory, manifest = loadReleaseManifest()) {
  const release = JSON.parse(await readFile(join(directory, "manifest.json"), "utf8"));
  if (release.schemaVersion !== 1 || release.partial || !/^\d+\.\d+\.\d+$/.test(release.version) ||
      release.tag !== `rust-v${release.version}` || !/^[a-f0-9]{40}$/.test(release.commit)) {
    throw new Error("only complete tagged stable standalone releases can be published");
  }
  const expected = manifest.platforms.flatMap((entry) => (entry.components?.length ? ["cpu", "cuda"] : ["cpu"])
    .map((accelerator) => `${entry.target}/${accelerator}`)).sort();
  if (JSON.stringify(release.assets.map((asset) => `${asset.target}/${asset.accelerator}`).sort()) !== JSON.stringify(expected)) {
    throw new Error("standalone release must cover every platform and variant exactly once");
  }
  const files = [];
  for (const asset of release.assets) {
    const entry = manifest.platforms.find((entry) => entry.target === asset.target);
    const expectedFile = `zvec-grep-${release.version}-${asset.target}-${asset.accelerator}.${entry.os === "win32" ? "zip" : "tar.gz"}`;
    if (asset.file !== expectedFile || !/^[a-f0-9]{64}$/.test(asset.sha256) ||
        (await stat(join(directory, asset.file))).size !== asset.bytes || await sha256File(join(directory, asset.file)) !== asset.sha256) {
      throw new Error(`standalone release payload mismatch: ${asset.file}`);
    }
    files.push(asset.file);
  }
  const expectedTable = `schema\t1\nversion\t${release.version}\ntag\t${release.tag}\n` +
    release.assets.map((asset) => `${asset.target}\t${asset.accelerator}\t${asset.file}\t${asset.sha256}\t${asset.bytes}\n`).join("");
  if (await readFile(join(directory, "manifest.tsv"), "utf8") !== expectedTable) throw new Error("shell and PowerShell manifests disagree");
  const checksums = await readFile(join(directory, "SHA256SUMS"), "utf8");
  const expectedChecksums = (await Promise.all([...files, ...metadata.filter((file) => file !== "SHA256SUMS")]
    .map(async (file) => `${await sha256File(join(directory, file))}  ${file}\n`))).join("");
  if (checksums !== expectedChecksums) throw new Error("standalone checksum inventory mismatch");
  return { release, files: [...files, ...metadata] };
}

export async function publishStandaloneRelease({ directory, repository = process.env.GITHUB_REPOSITORY,
  ref = process.env.GITHUB_REF, commit = process.env.GITHUB_SHA, gh = (args) => run("gh", args, { capture: true }) }) {
  const { release, files } = await verifyStandaloneRelease(directory);
  if (repository !== "zvec-ai/zvec-grep" || ref !== `refs/tags/${release.tag}` || commit !== release.commit) {
    throw new Error("standalone publication requires the official repository and the exact verified source tag/commit");
  }
  let existing;
  try { existing = JSON.parse(gh(["api", `repos/${repository}/releases/tags/${release.tag}`])); }
  catch (error) { if (!String(error.message).includes("404")) throw error; }
  const expectedAssets = await Promise.all(files.map(async (file) => ({ name: file,
    size: (await stat(join(directory, file))).size, digest: `sha256:${await sha256File(join(directory, file))}` })));
  const verifyRemote = (remote) => {
    if (remote.assets?.length !== expectedAssets.length || expectedAssets.some((expected) => !remote.assets.some((asset) =>
      asset.name === expected.name && asset.size === expected.size && asset.digest === expected.digest && asset.state === "uploaded"))) {
      throw new Error("GitHub release asset inventory or server-side digest mismatch; release remains unpublished");
    }
  };
  if (existing && !existing.draft) {
    verifyRemote(existing);
    return; // Never replace published bytes or move Latest backwards on a retry.
  }
  if (!existing) {
    const notes = join(directory, "release-notes.md");
    await writeFile(notes, `Standalone zvec-grep ${release.version}.\n\nInstall or upgrade on macOS/Linux:\n\n\`\`\`sh\ncurl -fsSL https://github.com/zvec-ai/zvec-grep/releases/latest/download/install.sh | sh\n\`\`\`\n\nWindows: download and run install.ps1 from this release. Use --version (PowerShell: -Version) to select a release. CUDA packages require compatible NVIDIA drivers and host CUDA/cuBLAS/cuDNN libraries.\n`);
    gh(["release", "create", release.tag, "--repo", repository, "--draft", "--verify-tag", "--title", `zvec-grep ${release.version}`, "--notes-file", notes]);
  }
  gh(["release", "upload", release.tag, "--repo", repository, "--clobber", ...files.map((file) => join(directory, file))]);
  verifyRemote(JSON.parse(gh(["api", `repos/${repository}/releases/tags/${release.tag}`])));
  gh(["release", "edit", release.tag, "--repo", repository, "--draft=false", "--latest", "--verify-tag"]);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  publishStandaloneRelease({ directory: resolve(process.argv[2] ?? join(workspaceDir, "dist/standalone")) }).catch((error) => {
    console.error(`standalone-publish: ${error.message}`);
    process.exitCode = 1;
  });
}
