#!/usr/bin/env python3
"""从构建机的锁定 Rust/前端依赖收集完整许可，补充上游漏装的许可和源码。"""

import argparse
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parent.parent
LICENSE_PREFIXES = ("license", "licence", "copying", "copyright", "notice", "ofl", "unlicense")


def copy_notices(directory, destination):
    found = []
    for path in directory.rglob("*"):
        # 只从当前包获取声明，不跟随其它 node_modules 包的符号链接。
        relative = path.relative_to(directory)
        if "node_modules" in relative.parts or path.is_symlink() or not path.is_file():
            continue
        if path.name.lower().startswith(LICENSE_PREFIXES):
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            found.append(relative.as_posix())
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    args.cache.mkdir(parents=True, exist_ok=True)
    download = runpy.run_path(str(ROOT / "tools/package-windows-release.py"))["download"]
    extras = json.loads((ROOT / "tools/third-party-license-sources.json").read_text(encoding="utf-8"))
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
        cwd=ROOT, text=True, encoding="utf-8"))
    resolved = {item["id"] for item in metadata["resolve"]["nodes"]}
    checksums = {}
    for block in (ROOT / "Cargo.lock").read_text(encoding="utf-8").split("[[package]]"):
        values = dict(re.findall(r'^(name|version|checksum) = "([^"\n]+)"', block, re.MULTILINE))
        if "checksum" in values:
            checksums[f"{values['name']}@{values['version']}"] = values["checksum"]
    index = []

    def supplement(key, destination, found):
        selected = None
        if not found:
            item = extras["packages"].get(key)
            if item is None:
                raise ValueError(f"依赖缺许可文件，须补充后再发行：{key}")
            destination.mkdir(parents=True, exist_ok=True)
            if "standard_license" in item:
                selected = item["standard_license"]
                item = extras["standard_licenses"][selected]
            shutil.copy2(download(item, args.cache), destination / item["filename"])
        return selected

    for package in metadata["packages"]:
        if package["id"] not in resolved or package["id"] in metadata["workspace_members"]:
            continue
        key = f"{package['name']}@{package['version']}"
        destination = output / "rust" / key
        directory = Path(package["manifest_path"]).parent
        found = copy_notices(directory, destination)
        selected = supplement(key, destination, found)
        source_url = f"https://static.crates.io/crates/{package['name']}/{package['name']}-{package['version']}.crate"
        # MPL 组件与缺少独立许可文件的组件额外附原版源码，保留其原版权声明。
        if "MPL" in (package["license"] or "") or selected:
            item = {"filename": f"{package['name']}-{package['version']}.crate", "url": source_url,
                    "sha256": checksums[key]}
            source_directory = output / "sources"
            source_directory.mkdir(exist_ok=True)
            shutil.copy2(download(item, args.cache), source_directory / item["filename"])
        index.append({"ecosystem": "Rust", "name": package["name"], "version": package["version"],
                      "license": package["license"], "authors": package["authors"],
                      "repository": package["repository"], "source": source_url,
                      "source_sha256": checksums[key], "selected_license": selected})
    command = (["cmd", "/d", "/s", "/c", "pnpm licenses list --prod --json"] if os.name == "nt"
               else ["pnpm", "licenses", "list", "--prod", "--json"])
    frontend = json.loads(subprocess.check_output(command, cwd=ROOT, text=True, encoding="utf-8"))
    seen = set()
    for group in frontend.values():
        for package in group:
            for path in package["paths"]:
                directory = Path(path)
                manifest = json.loads((directory / "package.json").read_text(encoding="utf-8"))
                key = f"{manifest['name']}@{manifest['version']}"
                if key in seen:
                    continue
                seen.add(key)
                destination = output / "frontend" / key.replace("/", "_")
                found = copy_notices(directory, destination)
                selected = supplement(key, destination, found)
                if selected:
                    source_directory = output / "sources"
                    source_directory.mkdir(exist_ok=True)
                    with zipfile.ZipFile(source_directory / (key.replace("/", "_") + ".zip"), "w", zipfile.ZIP_DEFLATED, strict_timestamps=False) as archive:
                        for source in directory.rglob("*"):
                            relative = source.relative_to(directory)
                            if source.is_file() and not source.is_symlink() and "node_modules" not in relative.parts:
                                archive.write(source, relative.as_posix())
                index.append({"ecosystem": "frontend", "name": manifest["name"], "version": manifest["version"],
                              "license": package["license"], "author": manifest.get("author"),
                              "repository": manifest.get("repository"), "selected_license": selected})
    (output / "index.json").write_text(json.dumps(index, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (output / "说明.txt").write_text(
        "本目录保留构建依赖原包的完整许可、版权及NOTICE，清单包含版本、作者和公开源码来源。\n"
        "Rust解析图也包含构建依赖，因此清单可能多于最终二进制实际链接的组件。\n"
        "缺少独立许可的上游包通过固定摘要补回原项目许可。fxhash/mac选择原声明允许的Apache-2.0；\n"
        "selectors附MPL-2.0全文和原版crate源码；dlv附MIT完整条款、原作者元数据及安装原包源码。\n"
        "标准许可模板中的占位符不替代原始版权：原始声明、作者和源码与其一并保留。\n"
        "所有MPL依赖的原版crate源码附于sources/，也可从index.json的公开地址免费获取。\n",
        encoding="utf-8")
    print(f"完整许可收集完成：Rust {len(index) - len(seen)} 项，前端 {len(seen)} 项")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    main()
