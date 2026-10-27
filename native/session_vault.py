"""会话材料的本地保护存储；Windows 使用当前用户 DPAPI，导出需要显式调用。"""
import ctypes
import json
import os
from pathlib import Path

MAGIC = b"MPIDER-SESSION-1\0"


def protect(data, decrypt=False):
    if os.name != "nt":
        raise RuntimeError("会话保护存储需要 Windows 当前用户环境")

    class Blob(ctypes.Structure):
        _fields_ = [("size", ctypes.c_ulong), ("data", ctypes.POINTER(ctypes.c_ubyte))]

    source = ctypes.create_string_buffer(data)
    incoming = Blob(len(data), ctypes.cast(source, ctypes.POINTER(ctypes.c_ubyte)))
    outgoing = Blob()
    crypt = ctypes.WinDLL("crypt32", use_last_error=True)
    crypt.CryptProtectData.argtypes = [ctypes.POINTER(Blob), ctypes.c_wchar_p,
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(Blob)]
    crypt.CryptUnprotectData.argtypes = [ctypes.POINTER(Blob), ctypes.c_void_p,
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(Blob)]
    if decrypt:
        ok = crypt.CryptUnprotectData(ctypes.byref(incoming), None, None, None, None,
                                     1, ctypes.byref(outgoing))
    else:
        ok = crypt.CryptProtectData(ctypes.byref(incoming), "MPider 微信会话", None, None,
                                   None, 1, ctypes.byref(outgoing))
    if not ok:
        raise RuntimeError("无法保护或读取当前用户的会话材料")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    try:
        return ctypes.string_at(outgoing.data, outgoing.size)
    finally:
        ctypes.memset(outgoing.data, 0, outgoing.size)
        kernel.LocalFree(outgoing.data)


def save(path, payload):
    path = Path(path)
    data = MAGIC + protect(json.dumps(payload, ensure_ascii=True).encode())
    temporary = path.with_suffix(".tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as handle:
        handle.write(data)
    os.replace(temporary, path)


def load(path):
    with Path(path).open("rb") as handle:
        data = handle.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("会话文件超过上限")
    if data.startswith(MAGIC):
        data = protect(data[len(MAGIC):], decrypt=True)
    return json.loads(data)
