/** 验证发布门禁：版本一致、平台完整、签名齐全，防止发布不可用的更新清单。 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createManifest, projectVersion, stageArtifacts, validateTag } from "./release-updater.mjs";

function temporary(run) {
  const root = mkdtempSync(join(tmpdir(), "mpider-release-test-"));
  try { run(root); } finally { rmSync(root, { recursive: true }); }
}
test("发布标签跟随当前项目版本，阻止标签与构建版本错配", () => {
  const version = projectVersion();
  assert.doesNotThrow(() => validateTag(version, `v${version}`));
  assert.throws(() => validateTag(version, "v99.0.0"), /不一致/);
  temporary(root => {
    mkdirSync(join(root, "src-tauri"));
    writeFileSync(join(root, "package.json"), JSON.stringify({ version: "1.0.0" }));
    writeFileSync(join(root, "src-tauri/tauri.conf.json"), JSON.stringify({ version: "0.1.0" }));
    writeFileSync(join(root, "Cargo.toml"), '[workspace.package]\nversion = "0.1.0"');
    assert.throws(() => projectVersion(root), /版本号必须一致/);
  });
});
test("Windows 产物生成可下载清单，不包含 macOS", () => temporary(root => {
  const staged = join(root, "staged");
  const version = projectVersion();
  for (const [target, filename] of [
    ["x86_64-pc-windows-msvc", `MPider_${version}_x64-setup.exe`],
  ]) {
    const dir = join(root, target); mkdirSync(dir);
    writeFileSync(join(dir, filename), "假安装包");
    writeFileSync(join(dir, `${filename}.sig`), `${target}-signature`);
    stageArtifacts(dir, staged, target, version, true);
  }
  const manifest = createManifest(staged, version, "areyoubugcoder/mpider");
  assert.equal(manifest.version, version);
  assert.deepEqual(Object.keys(manifest.platforms), ["windows-x86_64"]);
  assert.match(manifest.platforms["windows-x86_64"].url, /_x64-setup.exe$/);
  assert.equal(manifest.platforms["windows-x86_64"].signature, "x86_64-pc-windows-msvc-signature");
}));
test("缺包、缺签名和缺平台都阻止生成更新发布", () => temporary(root => {
  const version = projectVersion();
  assert.throws(() => stageArtifacts(root, join(root, "mac"), "aarch64-apple-darwin", version, true), /不支持/);
  assert.throws(() => createManifest(root, version, "areyoubugcoder/mpider"), /缺少/);
  assert.throws(() => stageArtifacts(root, join(root, "out"), "x86_64-pc-windows-msvc", version, true), /缺少/);
  writeFileSync(join(root, `MPider_${version}_x64-setup.exe`), "假安装包");
  assert.throws(() => stageArtifacts(root, join(root, "out"), "x86_64-pc-windows-msvc", version, true), /缺少签名/);
  assert.throws(() => createManifest(root, version, "areyoubugcoder/mpider"), /缺少/);
}));
