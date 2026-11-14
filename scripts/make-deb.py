#!/usr/bin/env python3
"""把 dist/linux-<arch> 的发布目录打包成 Debian .deb（无需 dpkg 环境）。

.deb 是一个 ar 归档：debian-binary + control.tar.gz + data.tar.gz。
本脚本用纯 Python 组装这三部分，产物结构符合 Debian Policy：
    control          包元数据（版本/架构/描述）
    postinst/postrm  安装/卸载后刷新应用菜单缓存
    /opt/snapfind/                 程序文件（自包含，无运行时依赖）
    /usr/share/applications/       桌面启动器
    /usr/share/icons/hicolor/256x256/apps/  图标

用法：
    python scripts/make-deb.py --src dist/linux-x64   --arch amd64 --out dist/SnapFind-linux-x64.deb   --version 0.2.0
    python scripts/make-deb.py --src dist/linux-arm64 --arch arm64 --out dist/SnapFind-linux-arm64.deb --version 0.2.0

之后可用 `wsl dpkg-deb -I <deb>` 或 `dpkg -I <deb>` 校验元数据。
"""

import argparse
import gzip
import io
import os
import tarfile

# 需要可执行位（0755）的文件；其余 0644
EXECUTABLES = {"SnapFind", "snapfind", "snapfind-indexd", "createdump", "postinst", "postrm"}

# 打进 /opt/snapfind 的内容中排除的辅助文件（deb 自身完成安装，不需要它们）
EXCLUDE = {"install.sh", "snapfind.desktop.template"}

DESKTOP_ENTRY = """\
[Desktop Entry]
Type=Application
Name=SnapFind
Comment=SnapFind local full-text search
Exec=/opt/snapfind/SnapFind
Path=/opt/snapfind
Icon=/usr/share/icons/hicolor/256x256/apps/snapfind.png
Terminal=false
Categories=Utility;
"""

POSTINST = """\
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications || true
fi
"""

POSTRM = """\
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications || true
fi
"""


def add_file(tar: tarfile.TarFile, name: str, data: bytes, mode: int):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    info.mtime = 0
    tar.addfile(info, io.BytesIO(data))


def add_dir(tar: tarfile.TarFile, name: str):
    """dpkg 只创建 data.tar 里**声明过的**目录，父目录条目必须逐级写全。"""
    info = tarfile.TarInfo(name.rstrip("/") + "/")
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    info.mtime = 0
    tar.addfile(info)


def add_existing(tar: tarfile.TarFile, src_path: str, arcname: str):
    with open(src_path, "rb") as f:
        data = f.read()
    mode = 0o755 if os.path.basename(src_path) in EXECUTABLES else 0o644
    add_file(tar, arcname, data, mode)


def build(src: str, arch: str, version: str, out: str):
    src = os.path.abspath(src)
    files = sorted(
        f for f in os.listdir(src)
        if os.path.isfile(os.path.join(src, f)) and f not in EXCLUDE
    )
    installed_kb = sum(os.path.getsize(os.path.join(src, f)) for f in files) // 1024 + 1

    # ---- control ----
    control = (
        f"Package: snapfind\n"
        f"Version: {version}\n"
        f"Section: utils\n"
        f"Priority: optional\n"
        f"Architecture: {arch}\n"
        f"Installed-Size: {installed_kb}\n"
        f"Maintainer: SnapFind <nbyh@users.noreply.github.com>\n"
        f"Description: Local full-text search for your files\n"
        f" SnapFind indexes file names and text content locally (Rust engine,\n"
        f" SQLite FTS5) and provides an instant GUI search. Fully self-contained:\n"
        f" no .NET or extra runtime required. Data stays on your machine.\n"
    ).encode()
    maint_scripts = {"postinst": POSTINST.encode(), "postrm": POSTRM.encode()}

    def write_tgz(add_entries) -> bytes:
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w") as tar:
            add_entries(tar)
        gz = io.BytesIO()
        with gzip.GzipFile(fileobj=gz, mode="wb", mtime=0) as g:
            g.write(buf.getvalue())
        return gz.getvalue()

    control_tgz = write_tgz(lambda tar: (
        add_file(tar, "control", control, 0o644),
        *[add_file(tar, n, d, 0o755) for n, d in maint_scripts.items()],
    ))

    def data_entries(tar: tarfile.TarFile):
        # 目录条目必须逐级声明（dpkg 不会隐式创建父目录）
        for d in ("opt/", "opt/snapfind/", "usr/", "usr/share/", "usr/share/applications/",
                  "usr/share/icons/", "usr/share/icons/hicolor/", "usr/share/icons/hicolor/256x256/",
                  "usr/share/icons/hicolor/256x256/apps/"):
            add_dir(tar, d)
        # /opt/snapfind/
        for f in files:
            add_existing(tar, os.path.join(src, f), f"opt/snapfind/{f}")
        # 桌面启动器 + 图标（hicolor 标准路径）
        add_file(tar, "usr/share/applications/snapfind.desktop", DESKTOP_ENTRY.encode(), 0o644)
        add_existing(tar, os.path.join(src, "snapfind.png"),
                     "usr/share/icons/hicolor/256x256/apps/snapfind.png")

    data_tgz = write_tgz(data_entries)

    # ---- ar 归档 ----
    def ar_member(name: str, data: bytes) -> bytes:
        # GNU ar 允许最长 15 字符的成员名（超过需 / 前缀扩展名，这里都够短）
        header = (
            f"{name:<16}"
            f"{0:<12}"
            f"{0:<6}"
            f"{0:<6}"
            f"{0o100644:<8}"
            f"{len(data):<10}"
            "`\n"
        ).encode()
        if len(data) % 2:
            data += b"\n"
        return header + data

    with open(out, "wb") as f:
        f.write(b"!<arch>\n")
        f.write(ar_member("debian-binary", b"2.0\n"))
        f.write(ar_member("control.tar.gz", control_tgz))
        f.write(ar_member("data.tar.gz", data_tgz))

    print(f"built {out} ({os.path.getsize(out) / 1_048_576:.1f} MB, arch={arch}, version={version})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--arch", required=True, choices=["amd64", "arm64"])
    ap.add_argument("--out", required=True)
    ap.add_argument("--version", required=True)
    args = ap.parse_args()
    build(args.src, args.arch, args.version, args.out)


if __name__ == "__main__":
    main()
