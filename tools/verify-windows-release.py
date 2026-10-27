#!/usr/bin/env python3
"""在 Windows 离线验证便携包完整性、依赖和虚构材料加密；不连接微信。"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("运行环境验收必须在 Windows x64 进行")
    directory = args.directory.resolve()
    manifest = json.loads((directory / "release-manifest.json").read_text(encoding="utf-8"))
    actual = {path.relative_to(directory).as_posix() for path in directory.rglob("*") if path.is_file()}
    if actual != set(manifest["files"]) | {"release-manifest.json"}:
        raise ValueError("发行目录存在缺失或额外文件")
    for name, expected in manifest["files"].items():
        path = (directory / name).resolve()
        if not path.is_relative_to(directory):
            raise ValueError("文件清单路径越界")
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise ValueError(f"发行文件摘要不匹配：{name}")
    data = (directory / "MPider.exe").read_bytes()
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if data[:2] != b"MZ" or data[offset:offset + 4] != b"PE\0\0" or struct.unpack_from("<H", data, offset + 4)[0] != 0x8664:
        raise ValueError("主程序不是 Windows x64 PE")
    # 只使用发行包自带 Python/依赖。所有加密材料均为临时虚构值。
    source = """
import json, sys, tempfile
from pathlib import Path
import frida, cryptography, cffi, pycparser
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
sys.path.insert(0, sys.argv[1])
from session_vault import load, save
key = AESGCM.generate_key(bit_length=128)
aes = AESGCM(key)
nonce, message = b'0' * 12, b'mpider-release-synthetic'
assert aes.decrypt(nonce, aes.encrypt(nonce, message, None), None) == message
with tempfile.TemporaryDirectory(prefix='mpider-release-check-') as temporary:
    path = Path(temporary) / 'synthetic.dat'
    value = {'synthetic': True, 'message': 'release-check'}
    save(path, value)
    assert load(path) == value
    assert b'release-check' not in path.read_bytes()
print(json.dumps({'python': '.'.join(map(str, sys.version_info[:3])),
 'frida': frida.__version__, 'cryptography': cryptography.__version__,
 'cffi': cffi.__version__, 'pycparser': pycparser.__version__,
 'aes_gcm': 'passed', 'dpapi': 'passed'}))
"""
    result = subprocess.run([str(directory / "runtime/python.exe"), "-I", "-B", "-c", source,
                             str(Path(__file__).resolve().parent.parent / "native")],
                            check=True, capture_output=True, text=True, timeout=60)
    runtime = json.loads(result.stdout)
    if runtime["python"] != manifest["python"]:
        raise ValueError("Python 版本与清单不符")
    for name, version in manifest["packages"].items():
        if runtime[name] != version:
            raise ValueError(f"运行依赖版本不符：{name}")
    licenses = json.loads((directory / "licenses/dependencies/index.json").read_text(encoding="utf-8"))
    for name in ("@fontsource-variable/inter", "@fontsource-variable/jetbrains-mono"):
        if not any(item["name"] == name for item in licenses):
            raise ValueError(f"依赖许可清单缺字体：{name}")
    evidence = {"version": manifest["version"], "source_revision": manifest["source_revision"],
                "files_verified": len(manifest["files"]), "dependency_notices": len(licenses), "runtime": runtime,
                "scope": "离线完整性与虚构材料；未启动应用、连接微信或采集"}
    (directory.parent / "verification.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(evidence, ensure_ascii=False))


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    main()
