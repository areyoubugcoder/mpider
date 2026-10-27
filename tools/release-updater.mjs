/** 发布前核对项目版本，整理 Windows 签名产物，并生成 GitHub 静态更新清单。 */
import { readFileSync, writeFileSync, mkdirSync, readdirSync, copyFileSync } from "node:fs";
import { join, basename } from "node:path";
import { pathToFileURL } from "node:url";

const targets = {
  "x86_64-pc-windows-msvc": { key: "windows-x86_64", suffix: "-setup.exe" },
};
export function projectVersion(root = ".") {
  const version = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"))).version;
  const frontend = JSON.parse(readFileSync(join(root, "package.json"))).version;
  const cargo = readFileSync(join(root, "Cargo.toml"), "utf8").split("[workspace.package]")[1]?.split(/^\[/m)[0]?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (version !== frontend || version !== cargo) throw new Error("package.json、Cargo.toml 与 tauri.conf.json 版本号必须一致");
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error("正式发布必须使用稳定语义版本号");
  return version;
}
export function validateTag(version, tag) {
  if (tag !== `v${version}`) throw new Error(`标签 ${tag} 与当前项目版本 v${version} 不一致`);
}
function files(root) {
  return readdirSync(root, { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? files(join(root, entry.name)) : [join(root, entry.name)]);
}
export function stageArtifacts(root, destination, target, version, requireSignature) {
  const platform = targets[target];
  if (!platform) throw new Error(`不支持的发布平台：${target}`);
  mkdirSync(destination, { recursive: true });
  const entries = files(root);
  const packages = entries.filter(path => /\.(msi|exe)$/.test(path));
  if (!packages.some(path => path.endsWith(platform.suffix))) throw new Error(`缺少 ${target} 更新安装包`);
  for (const path of packages) {
    const name = basename(path);
    copyFileSync(path, join(destination, name));
    const signature = `${path}.sig`;
    if (entries.includes(signature)) copyFileSync(signature, join(destination, `${name}.sig`));
    else if (requireSignature && path.endsWith(platform.suffix)) throw new Error(`缺少签名：${name}.sig`);
  }
}
export function createManifest(root, version, repository, notes = "") {
  const entries = readdirSync(root);
  const platforms = {};
  for (const platform of Object.values(targets)) {
    const matches = entries.filter(name => name.endsWith("_x64-setup.exe") && name.includes(`_${version}_`));
    if (matches.length !== 1) throw new Error(`缺少或存在多个 ${platform.key} 安装包`);
    const name = matches[0];
    if (!entries.includes(`${name}.sig`)) throw new Error(`缺少签名：${name}.sig`);
    const signature = readFileSync(join(root, `${name}.sig`), "utf8").trim();
    if (!signature) throw new Error(`签名为空：${name}.sig`);
    platforms[platform.key] = { signature, url: `https://github.com/${repository}/releases/download/v${version}/${encodeURIComponent(name)}` };
  }
  return { version, notes, pub_date: new Date().toISOString(), platforms };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [command, ...args] = process.argv.slice(2);
  const version = projectVersion();
  if (command === "prepare") {
    const tag = process.env.GITHUB_REF_TYPE === "tag";
    if (tag) validateTag(version, process.env.GITHUB_REF_NAME);
    const publicKey = process.env.MPIDER_UPDATER_PUBLIC_KEY?.trim();
    const privateKey = process.env.TAURI_SIGNING_PRIVATE_KEY?.trim();
    if (tag && (!publicKey || !privateKey)) throw new Error("发布签名更新需要配置 MPIDER_UPDATER_PUBLIC_KEY 和 TAURI_SIGNING_PRIVATE_KEY");
    if (publicKey && privateKey) {
      const path = "src-tauri/tauri.conf.json";
      const config = JSON.parse(readFileSync(path));
      config.plugins.updater.pubkey = publicKey;
      config.bundle.createUpdaterArtifacts = true;
      writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`);
    }
    console.log(`已核对当前项目版本 v${version}；${publicKey && privateKey ? "启用签名更新产物" : "仅构建普通安装包"}`);
  } else if (command === "stage") {
    const [root, destination, target] = args;
    stageArtifacts(root, destination, target, version, process.env.GITHUB_REF_TYPE === "tag");
  } else if (command === "manifest") {
    const [root, repository] = args;
    validateTag(version, process.env.GITHUB_REF_NAME);
    writeFileSync(join(root, "latest.json"), `${JSON.stringify(createManifest(root, version, repository, `MPider v${version}，详细变更请查看 GitHub Release。`), null, 2)}\n`);
  } else throw new Error("用法：release-updater.mjs prepare | stage <目录> <输出> <target> | manifest <目录> <仓库>");
}
