#!/usr/bin/env python3
"""组装固定依赖的 Windows x64 便携发行包；不运行微信、不读取用户数据。"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent.parent
# 发行资料逐项列出；源码 archive 没有 .git，不能递归复制工作目录中的私有材料。
PUBLIC_DOCUMENTS = (
    "docs/windows-standalone.md",
    "docs/how-it-works.md",
    "docs/faq.md",
    "docs/laboratory.md",
    "docs/request-policy.md",
    "docs/advertisements.md",
    "docs/article-export.md",
    "docs/source-deletion.md",
    "docs/app-updates.md",
    "docs/system-logs.md",
    "docs/wechat-session-protocol.md",
    "docs/screenshots/workbench-sources.png",
    "docs/screenshots/workbench-articles.png",
    "docs/screenshots/workbench-tasks.png",
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def download(item, cache):
    path = cache / (item["sha256"][:16] + "-" + item["filename"])
    if path.is_file() and digest(path) == item["sha256"]:
        return path
    for attempt in range(3):
        try:
            with urllib.request.urlopen(item["url"], timeout=120) as response:
                data = response.read()
            if hashlib.sha256(data).hexdigest() != item["sha256"]:
                raise ValueError(f"下载摘要不匹配：{item['filename']}")
            path.write_bytes(data)
            return path
        except OSError:
            if attempt == 2:
                raise
            time.sleep(2)


def extract(archive, directory):
    with zipfile.ZipFile(archive) as bundle:
        for item in bundle.infolist():
            target = (directory / item.filename).resolve()
            if not target.is_relative_to(directory.resolve()):
                raise ValueError("压缩包路径越界")
        bundle.extractall(directory)


def verify_pe(executable):
    data = executable.read_bytes()
    if data[:2] != b"MZ" or len(data) < 64:
        raise ValueError("程序不是 Windows PE 文件")
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if data[offset:offset + 4] != b"PE\0\0" or struct.unpack_from("<H", data, offset + 4)[0] != 0x8664:
        raise ValueError("程序不是 Windows x64 PE 文件")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "dist-windows")
    parser.add_argument("--revision", default="source-archive")
    args = parser.parse_args()
    verify_pe(args.exe)
    version = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))["version"]
    lock = json.loads((ROOT / "tools/windows-runtime.lock.json").read_text(encoding="utf-8"))
    for item in lock["packages"]:
        if item["name"] in ("frida", "cryptography"):
            requirement = f"{item['name']}=={item['version']}"
            if requirement not in (ROOT / "native/requirements.txt").read_text(encoding="utf-8").splitlines():
                raise ValueError(f"运行依赖与锁定包不一致：{requirement}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    package_name = f"MPider-v{version}-windows-x64"
    stage = output / package_name
    if stage.exists():
        shutil.rmtree(stage)
    runtime = stage / "runtime"
    runtime.mkdir(parents=True)
    cache = output / ".downloads"
    cache.mkdir(exist_ok=True)
    extract(download(lock["python"], cache), runtime)
    # ._pth 维持嵌入式隔离；加入发行包内的依赖，忽略全局 Python/site-packages。
    (runtime / "python312._pth").write_text("python312.zip\n.\nsession-deps\n", encoding="utf-8")
    dependencies = runtime / "session-deps"
    dependencies.mkdir()
    for item in lock["packages"]:
        extract(download(item, cache), dependencies)
    licenses = stage / "licenses"
    licenses.mkdir()
    for item in lock["licenses"]:
        shutil.copy2(download(item, cache), licenses / item["filename"])
    subprocess.run([sys.executable, str(ROOT / "tools/collect-third-party-licenses.py"),
                    "--output", str(licenses / "dependencies"), "--cache", str(cache)], check=True)
    # wheel 原有的完整 LICENSE/COPYING 及元数据原样留在 runtime/session-deps。
    required_licenses = [
        runtime / "LICENSE.txt",
        dependencies / "frida-17.23.3.dist-info/licenses/COPYING",
        dependencies / "cryptography-50.0.2.dist-info/licenses/LICENSE.APACHE",
        dependencies / "cryptography-50.0.2.dist-info/licenses/LICENSE.BSD",
        dependencies / "cffi-2.0.0.dist-info/licenses/LICENSE",
        dependencies / "pycparser-2.23.dist-info/LICENSE",
        licenses / "Frida-COPYING.LIB",
    ]
    if any(not path.is_file() for path in required_licenses):
        raise ValueError("运行环境缺少完整许可证")
    shutil.copy2(args.exe, stage / "MPider.exe")
    for filename in ("LICENSE", "THIRD-PARTY-NOTICES.md", "README.md"):
        shutil.copy2(ROOT / filename, stage / filename)
    for filename in ("CHANGELOG.md", "SECURITY.md", "CONTRIBUTING.md"):
        if (ROOT / filename).is_file():
            shutil.copy2(ROOT / filename, stage / filename)
    # 只带已审阅文档与三张无真实数据的演示截图；缺文件即停止，不回退旧截图。
    for document_name in PUBLIC_DOCUMENTS:
        path = ROOT / document_name
        if not path.is_file() or path.is_symlink() or not path.resolve().is_relative_to(ROOT.resolve()):
            raise ValueError(f"发行公开资料缺失或路径不合法：{document_name}")
        destination = stage / document_name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    shutil.copy2(ROOT / "tools/windows-runtime.lock.json", stage / "runtime-lock.json")
    (stage / "使用说明.txt").write_text(
        "MPider Windows x64 便携版\n\n"
        "完整解压到可写目录，双击 MPider.exe；不要只复制 EXE。\n"
        "依赖 Windows 10/11 x64、Microsoft WebView2、已登录的匹配版本微信。\n"
        "已附 Python/Frida，无需安装 Node.js、pnpm、Rust 或系统 Python。\n"
        "设置中的 Python 路径保留默认 python，即使用旁边 runtime/python.exe。\n"
        "当前工作台不要求安装根证书或修改系统代理。\n"
        "此程序未代码签名；首次识别依赖微信版本，Frida 仍可能引发微信崩溃。\n"
        "仅在自己有权访问的范围内使用；请求预算和冷却不保证账号不会受限制。\n"
        "关闭窗口进入托盘，从托盘退出才停止程序。\n"
        "反馈：https://github.com/areyoubugcoder/mpider/issues/new/choose\n",
        encoding="utf-8-sig",
    )
    files = {path.relative_to(stage).as_posix(): digest(path)
             for path in sorted(stage.rglob("*")) if path.is_file()}
    manifest = {"version": version, "target": "x86_64-pc-windows-msvc",
                "source_revision": args.revision, "python": lock["python"]["version"],
                "packages": {item["name"]: item["version"] for item in lock["packages"]},
                "files": files}
    (stage / "release-manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    archive = output / f"{package_name}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, strict_timestamps=False) as bundle:
        for path in sorted(stage.rglob("*")):
            if path.is_file():
                bundle.write(path, path.relative_to(stage).as_posix())
    (output / "SHA256SUMS").write_text(f"{digest(archive)}  {archive.name}\n", encoding="ascii")
    print(f"便携包：{archive}")
    print(f"文件数：{len(files)}；SHA-256：{digest(archive)}")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    main()
