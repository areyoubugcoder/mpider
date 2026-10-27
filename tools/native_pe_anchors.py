#!/usr/bin/env python3
"""只读定位 PE 中的 JSSDK 字符串引用及函数边界；依赖 pefile。"""

import argparse
import bisect
import hashlib
import json
import re
import struct
from pathlib import Path

import pefile


def analyze(path):
    data = Path(path).read_bytes()
    pe = pefile.PE(data=data, fast_load=True)
    text = next(s for s in pe.sections if s.Name.startswith(b".text"))
    pdata = next(s for s in pe.sections if s.Name.startswith(b".pdata")).get_data()
    functions = sorted((a, b) for a, b, _ in struct.iter_unpack("<III", pdata[:len(pdata) // 12 * 12])
                       if a and b > a)
    starts = [a for a, _ in functions]
    targets = {}
    for name in ("H5ExtTransfer", "finderH5ExtTransfer", "__callback_id", "__msg_type"):
        pos = 0
        while True:
            pos = data.find(name.encode(), pos)
            if pos < 0:
                break
            targets.setdefault(pe.get_rva_from_offset(pos), []).append(name)
            pos += len(name)
    code = text.get_data()
    refs = []
    pattern = rb"[\x48\x4c][\x8d\x8b][\x05\x0d\x15\x1d\x25\x2d\x35\x3d][\s\S]{4}"
    for match in re.finditer(pattern, code):
        rva = text.VirtualAddress + match.start()
        target = rva + 7 + struct.unpack_from("<i", code, match.start() + 3)[0]
        if target not in targets:
            continue
        index = bisect.bisect_right(starts, rva) - 1
        function = functions[index] if index >= 0 and functions[index][1] > rva else None
        refs.append({"names": targets[target], "xref": hex(rva), "target": hex(target),
                     "function": [hex(v) for v in function] if function else None})
    return {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data),
            "image_base": hex(pe.OPTIONAL_HEADER.ImageBase), "references": refs}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dll")
    parser.add_argument("--out")
    args = parser.parse_args()
    result = analyze(args.dll)
    rendered = json.dumps(result, ensure_ascii=False, indent=2)
    if args.out:
        Path(args.out).write_text(rendered)
    print(rendered)
