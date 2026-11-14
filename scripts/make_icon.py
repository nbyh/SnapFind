"""生成 256x256 应用图标：深蓝圆角方块 + 白色放大镜（纯 stdlib，无需 PIL）。

设计对齐 build-release.ps1 的 GDI+ 版本：蓝色渐变圆角方块，
白色粗描边放大镜（圆环 + 柄）。4x 超采样抗锯齿。
"""
import math
import struct
import sys
import zlib

S = 256          # 输出尺寸
SS = 4           # 超采样倍数
N = S * SS
R = 80 * SS      # 圆角半径
RING_W = 18 * SS / 2  # 描边半宽

CX, CY = 102 * SS, 102 * SS   # 放大镜圆心
RAD = 40 * SS                 # 圆环半径
H1 = (134 * SS, 134 * SS)     # 柄起点
H2 = (186 * SS, 186 * SS)     # 柄终点


def rounded_square_dist(x, y):
    """到圆角方块内部的符号距离（<0 在内部）。方块 [0,256]^2，圆角 R。"""
    cx = min(max(x, R), N - R)
    cy = min(max(y, R), N - R)
    if R <= x <= N - R or R <= y <= N - R:
        return max(R - x, x - (N - R), R - y, y - (N - R))
    return math.hypot(x - cx, y - cy) - R


def seg_dist(px, py, a, b):
    ax, ay = a
    bx, by = b
    vx, vy = bx - ax, by - ay
    t = ((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)
    t = min(1.0, max(0.0, t))
    return math.hypot(px - (ax + t * vx), py - (ay + t * vy))


def color_at(x, y):
    d = rounded_square_dist(x, y)
    if d >= 0:
        return None
    # 蓝色渐变 (30,90,200) -> (20,50,130)
    t = (x + y) / (2 * N)
    r = 30 + (20 - 30) * t
    g = 90 + (50 - 90) * t
    b = 200 + (130 - 200) * t
    # 放大镜：圆环或柄
    ring = abs(math.hypot(x - CX, y - CY) - RAD)
    handle = seg_dist(x, y, H1, H2)
    if ring < RING_W or handle < RING_W:
        return (255, 255, 255)
    return (int(r), int(g), int(b))


def main(path):
    # 下采样：对每个输出像素取 SS x SS 样本平均（未命中计透明）
    rows = []
    for oy in range(S):
        row = bytearray([0])  # PNG filter 0
        for ox in range(S):
            rs = gs = bs = as_ = 0
            for sy in range(SS):
                for sx in range(SS):
                    c = color_at(ox * SS + sx + 0.5, oy * SS + sy + 0.5)
                    if c:
                        rs += c[0]; gs += c[1]; bs += c[2]; as_ += 255
            n = SS * SS
            cnt = as_ // 255  # 命中样本数（决定覆盖度/alpha）
            if cnt:
                row += bytes((rs // cnt, gs // cnt, bs // cnt, as_ // n))
        rows.append(bytes(row))

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    ihdr = struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0)
    idat = zlib.compress(b"".join(rows), 9)
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
           + chunk(b"IDAT", idat) + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)
    print(f"written {path} ({len(png)} bytes)")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "snapfind.png")
