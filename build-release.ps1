# =====================================================================
# SnapFind 四平台发布脚本
#
#   .\build-release.ps1                    # 构建全部四个平台包
#   .\build-release.ps1 -Only win-arm64    # 只构建某个平台（增量调试用）
#   .\build-release.ps1 -SkipClean         # 不清理 dist（保留旧包）
#
# 产物：dist\<rid>\ 目录 + dist\SnapFind-<rid>.(zip|tar.gz) 压缩包
#   win-x64      本机 cargo 编译（MSVC x64）
#   win-arm64    cargo-zigbuild + zig 交叉编译（MSVC ARM64）
#   linux-x64    cargo-zigbuild + zig，musl 静态链接（任意发行版可跑）
#   linux-arm64  同上
#
# 依赖工具（脚本自动检查并补齐，装好后持久保留）：
#   * zig            pip 安装 ziglang==0.13.0，zig.exe 复制到 ~/.cargo/bin
#                    （0.13 是 cargo-zigbuild 支持的上限版本，勿升级）
#   * cargo-zigbuild cargo install cargo-zigbuild --locked
#   * .NET 8 SDK     dotnet publish 交叉 RID 需要 NuGet 可联网
# =====================================================================
param(
    [string]$Only = "",
    [switch]$SkipClean
)

$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot
$Dist = Join-Path $Root 'dist'
$CargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

# ---- 依赖工具：检查并补齐（持久化，之后重跑不再下载） -----------------

function Ensure-Tools {
    # 1. zig（经 pip，版本必须钉在 0.13.x）
    $zigVer = (& python -m ziglang version 2>$null)
    if ($LASTEXITCODE -ne 0 -or -not ($zigVer -match '^0\.13\.')) {
        Write-Host "安装 ziglang==0.13.0（当前：$zigVer）..."
        pip install "ziglang==0.13.0" 2>&1 | Select-String 'Successfully|error' | Write-Host
    }
    # zig.exe 必须留在 site-packages（按自身位置定位 lib 目录，复制走会失效），
    # 把该目录加进 PATH 供 cargo-zigbuild 使用。安装路径由 python 推导，
    # 不再硬编码 C:\Python312（不同机器的 Python 安装位置不同）
    $zigHome = (& python -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))" 2>$null)
    if (-not $zigHome -or -not (Test-Path (Join-Path $zigHome 'zig.exe'))) {
        throw "找不到 zig.exe：请确认 pip install ziglang==0.13.0 已完成（推导路径：$zigHome）"
    }
    $env:PATH += ";$zigHome"

    # 2. cargo-zigbuild
    if (-not (Test-Path (Join-Path $CargoBin 'cargo-zigbuild.exe'))) {
        Write-Host "安装 cargo-zigbuild..."
        cargo install cargo-zigbuild --locked 2>&1 | Select-String 'error|Installed' | Write-Host
    }
    $env:PATH += ";$CargoBin"
    Write-Host "zig = $((& (Join-Path $zigHome 'zig.exe') version))"
}

Ensure-Tools

# ---- 目标矩阵 ----------------------------------------------------------

# win-arm64 用 gnullvm（mingw 链接）：msvc 目标会撞上 zig 0.13 链接期
# InvalidWtf8 的 bug，gnullvm 走 zig cc -target aarch64-windows-gnu 全程无碍
$Targets = @(
    @{ Rid = 'win-x64';     Triple = '';                              Exe = 'exe' },
    @{ Rid = 'win-arm64';   Triple = 'aarch64-pc-windows-gnullvm';    Exe = 'exe' },
    @{ Rid = 'linux-x64';   Triple = 'x86_64-unknown-linux-musl';     Exe = '' },
    @{ Rid = 'linux-arm64'; Triple = 'aarch64-unknown-linux-musl';    Exe = '' }
)
if ($Only) { $Targets = $Targets | Where-Object { $_.Rid -eq $Only } }
if (-not $Targets) { throw "未知平台：$Only（可选 win-x64 / win-arm64 / linux-x64 / linux-arm64）" }

# ---- 清理输出目录 ------------------------------------------------------

if (-not $SkipClean) {
    # 逐文件删除再删目录：整体 -Recurse 偶发被文件占用/批删保护打断
    if (Test-Path $Dist) {
        foreach ($f in Get-ChildItem $Dist -File -Recurse -Force) { Remove-Item -LiteralPath $f.FullName -Force }
        $dirs = Get-ChildItem $Dist -Directory -Recurse -Force | Sort-Object { $_.FullName.Length } -Descending
        foreach ($d in $dirs) { Remove-Item -LiteralPath $d.FullName -Force }
        Remove-Item -LiteralPath $Dist -Force
    }
    Write-Host "已清理 $Dist"
}

$stamp = Get-Date -Format 'yyyy-MM-dd HH:mm'

foreach ($t in $Targets) {
    $rid = $t.Rid
    $triple = $t.Triple
    $out = Join-Path $Dist $rid
    Write-Host "`n===== [$rid] 引擎（$($triple ? $triple : '本机 MSVC')) ====="

    # 1. Rust 引擎 + CLI（CLI 包名 snapfind-cli，二进制名 snapfind）
    if ($triple) {
        cargo zigbuild --release -p snapfind-indexd -p snapfind-cli --target $triple
    } else {
        cargo build --release -p snapfind-indexd -p snapfind-cli
    }
    if ($LASTEXITCODE -ne 0) { throw "[$rid] 引擎编译失败" }

    # 2. 桌面端（自包含，目标机器无需装 .NET）
    #    Linux 上程序集改名 SnapFind：默认名 SnapFind.Desktop 以 .Desktop 结尾，
    #    会被文件管理器识别成 freedesktop 启动器配置文件（*.desktop）而非可执行
    #    程序，双击无法运行。改名必须走 AssemblyName（宿主按自身文件名找同名
    #    dll，发布后手动改名会破坏 apphost/deps.json 的对应关系）。
    Write-Host "===== [$rid] 桌面端 publish ====="
    $pubArgs = @(
        'publish', (Join-Path $Root 'SnapFind.Desktop\SnapFind.Desktop.csproj'),
        '-c', 'Release', '-r', $rid, '--self-contained', 'true',
        '-o', $out, '--nologo'
    )
    if ($rid -like 'linux-*') { $pubArgs += '-p:AssemblyName=SnapFind' }
    # publish 偶发失败（疑似杀软扫描刚写出的文件导致的瞬时锁），重试 3 次。
    # 输出全程可见；失败时额外回显最后 15 行，避免「第 N 次失败」却找不到原因
    $pubOk = $false
    $pubLog = @()
    for ($try = 1; $try -le 3 -and -not $pubOk; $try++) {
        if ($try -gt 1) {
            Write-Host "[$rid] publish 第 $($try - 1) 次失败，3 秒后重试…"
            Start-Sleep -Seconds 3
        }
        $pubLog = & dotnet @pubArgs 2>&1 | Out-String -Stream
        $code = $LASTEXITCODE
        $pubLog | ForEach-Object { Write-Host $_ }
        $pubOk = ($code -eq 0)
    }
    if (-not $pubOk) {
        Write-Host "---- publish 最后输出 ----"
        $pubLog | Select-Object -Last 15 | ForEach-Object { Write-Host $_ }
        throw "[$rid] 桌面端发布失败（重试 3 次均失败，真实错误见上方输出）"
    }

    # 3. 复制原生二进制
    $binDir = if ($triple) { Join-Path $Root "target\$triple\release" } else { Join-Path $Root 'target\release' }
    $suffix = if ($t.Exe) { '.exe' } else { '' }
    Copy-Item (Join-Path $binDir "snapfind-indexd$suffix") $out -Force
    Copy-Item (Join-Path $binDir "snapfind$suffix") $out -Force

    # 3.5 Linux 附带：安装脚本（R4-3）、256×256 图标、桌面启动器模板。
    #     `./install.sh` 安装到 ~/.local/opt/snapfind 并注册应用菜单；
    #     不想用脚本时也可手动解压后按 snapfind.desktop.template 配置。
    if ($rid -like 'linux-*') {
        # 生成 256×256 图标（纯 GDI+ 绘制：深蓝圆角方块 + 白色放大镜）
        $pngPath = Join-Path $out 'snapfind.png'
        Add-Type -AssemblyName System.Drawing
        $bmp = New-Object System.Drawing.Bitmap 256, 256
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.SmoothingMode = 'AntiAlias'
        $g.Clear([System.Drawing.Color]::Transparent)
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            (New-Object System.Drawing.Point 0, 0), (New-Object System.Drawing.Point 256, 256),
            [System.Drawing.Color]::FromArgb(255, 30, 90, 200), [System.Drawing.Color]::FromArgb(255, 20, 50, 130))
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.AddArc(0, 0, 80, 80, 180, 90); $path.AddArc(176, 0, 80, 80, 270, 90)
        $path.AddArc(176, 176, 80, 80, 0, 90); $path.AddArc(0, 176, 80, 80, 90, 90)
        $path.CloseFigure()
        $g.FillPath($brush, $path)
        $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), 18
        $g.DrawEllipse($pen, 62, 62, 80, 80)
        $g.DrawLine($pen, 134, 134, 186, 186)
        $g.Dispose(); $brush.Dispose(); $path.Dispose(); $pen.Dispose()
        $bmp.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()

        $installSh = @'
#!/bin/sh
# SnapFind Linux 安装脚本（用户级，无需 root）
# 用法：解压后在本目录执行 ./install.sh
set -e
DEST="$HOME/.local/opt/snapfind"
APPS="$HOME/.local/share/applications"
mkdir -p "$DEST" "$APPS"
cp -f SnapFind snapfind-indexd snapfind snapfind.png manifest.txt "$DEST"/ 2>/dev/null || true
cp -f SnapFind snapfind-indexd snapfind snapfind.png manifest.txt "$DEST"/
chmod +x "$DEST/SnapFind" "$DEST/snapfind-indexd" "$DEST/snapfind"
cat > "$APPS/snapfind.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SnapFind
Comment=SnapFind local full-text search
Exec=$DEST/SnapFind
Path=$DEST
Icon=$DEST/snapfind.png
Terminal=false
Categories=Utility;
EOF
echo "已安装到 $DEST，应用菜单项已写入 $APPS/snapfind.desktop"
echo "卸载：删除上述两个路径即可"
'@
        [IO.File]::WriteAllText((Join-Path $out 'install.sh'), ($installSh -replace "`r`n", "`n"))
        $launcher = @"
[Desktop Entry]
Type=Application
Name=SnapFind
Comment=SnapFind local full-text search
# 手动安装时按实际解压路径改 Exec/Path/Icon（或直接用包内 install.sh）
Exec=/opt/snapfind/SnapFind
Path=/opt/snapfind
Icon=/opt/snapfind/snapfind.png
Terminal=false
Categories=Utility;
"@
        [IO.File]::WriteAllText((Join-Path $out 'snapfind.desktop.template'), $launcher)
    }

    # 4. 版本清单
    $cargoVer = ((Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern 'version = "(.+)"').Matches[0].Groups[1].Value)
    $app = if ($rid -like 'linux-*') { 'SnapFind' } else { 'SnapFind.Desktop.exe' }
    $manifest = @"
SnapFind 发布包 [$rid]
版本     : $cargoVer
构建时间 : $stamp
引擎     : snapfind-indexd（Rust，$(if ($rid -like 'linux-*') { 'musl 静态链接' } else { 'MSVC' })）
桌面端   : $app（.NET 8 自包含）
用法     : 运行 ./$app；CLI 管理命令见 ./snapfind --help
$(if ($rid -like 'linux-*') { "桌面启动 : ./install.sh 一键安装（或参见 snapfind.desktop.template）" })
"@
    Set-Content -Encoding UTF8 (Join-Path $out 'manifest.txt') $manifest

    # 5. 压缩包：Windows 用 zip，Linux 用 tar.gz（保留可执行位）
    if ($rid -like 'win-*') {
        Compress-Archive -Path "$out\*" -DestinationPath (Join-Path $Dist "SnapFind-$rid.zip") -Force
    } else {
        Push-Location $Dist
        tar -czf "SnapFind-$rid.tar.gz" "$rid"
        Pop-Location
    }

    # 5.5 Debian 包（.deb）：纯 Python 组装（scripts/make-deb.py），无 dpkg 环境也能打；
    #     结构已用 dpkg-deb -I/-c 与 WSL 实装验证。产物装到 /opt/snapfind 并注册应用菜单。
    if ($rid -like 'linux-*') {
        $debArch = if ($rid -eq 'linux-x64') { 'amd64' } else { 'arm64' }
        python (Join-Path $Root 'scripts\make-deb.py') --src $out --arch $debArch `
            --out (Join-Path $Dist "SnapFind-$rid.deb") --version $cargoVer
        if ($LASTEXITCODE -ne 0) { throw "[$rid] deb 打包失败" }
    }
    Write-Host "[$rid] 完成 → $out"
}

# ---- 校验和：dist\SHA256SUMS.txt 覆盖全部压缩包 -----------------------
# Linux 侧可直接 `sha256sum -c SHA256SUMS.txt`（行格式与 GNU sha256sum 兼容）
$sums = Get-ChildItem $Dist -File | Where-Object { $_.Name -match '\.(zip|tar\.gz|deb)$' } |
    ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $($_.Name)"
    }
Set-Content -Encoding ASCII (Join-Path $Dist 'SHA256SUMS.txt') ($sums -join "`n")
Write-Host "`n校验和已生成：$Dist\SHA256SUMS.txt"

Write-Host "`n全部完成。产物在 $Dist"
Get-ChildItem $Dist | Select-Object Name, Length | Format-Table -AutoSize
