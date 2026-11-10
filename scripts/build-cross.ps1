# 交叉构建 Rust 侧二进制：Linux x64/arm64（glibc）与 Windows arm64（gnullvm）
# 依赖：cargo-zigbuild + zig，产物落入 dist\<rid>\ 与 .NET 桌面端合并
param(
    [string]$Root = "F:\Code\CodeBuddy\SnapFind",
    [long]$ZigBytes = 100343666,
    [string]$ZigUrl = "https://ziglang.org/download/0.17.1/zig-x86_64-windows-0.17.1.zip"
)

$ErrorActionPreference = "Stop"
Set-Location $Root
$log = Join-Path $Root ".tools\cross-build.log"
New-Item -ItemType Directory -Force ".tools" | Out-Null
function Log([string]$m) {
    $line = "$(Get-Date -Format 'HH:mm:ss') $m"
    Write-Host $line
    Add-Content -Path $log -Value $line
}

$zip = Join-Path $Root ".tools\zig.zip"
$zigDir = Join-Path $Root ".tools\zig-x86_64-windows-0.17.1"
$zigExe = Join-Path $zigDir "zig.exe"

# 1) 等 zig 就绪：下载未完则续传，完成后解压
$guard = 0
while (-not (Test-Path $zigExe)) {
    if ($guard -gt 720) { Log "abort: zig 等待超时"; exit 1 }
    $guard++
    $size = if (Test-Path $zip) { (Get-Item $zip).Length } else { 0 }
    if ($size -ge $ZigBytes) {
        Log "zip 完整（$([math]::Round($size/1MB,1))MB），解压"
        try {
            Expand-Archive -Path $zip -DestinationPath (Join-Path $Root ".tools") -Force
            Remove-Item $zip -Force
        } catch {
            Log "解压失败，回退重下：$($_.Exception.Message)"
            Remove-Item $zip -Force -ErrorAction SilentlyContinue
            curl.exe -sS -L -o $zip $ZigUrl
        }
        continue
    }
    if (-not (Get-Process -Name curl -ErrorAction SilentlyContinue)) {
        Log "续传下载（当前 $([math]::Round($size/1MB,1))MB / $([math]::Round($ZigBytes/1MB,1))MB）"
        curl.exe -sS -L -C - -o $zip $ZigUrl
        continue
    }
    Log "等待下载 $([math]::Round($size/1MB,2))MB / $([math]::Round($ZigBytes/1MB,1))MB"
    Start-Sleep -Seconds 15
}
Log "zig 就绪：$(& $zigExe version)"
$env:PATH = "$zigDir;$env:PATH"

# 2) 构建三个目标
$targets = @(
    @{ name = "x86_64-unknown-linux-gnu";  rid = "linux-x64";   ext = "" },
    @{ name = "aarch64-unknown-linux-gnu"; rid = "linux-arm64"; ext = "" },
    @{ name = "aarch64-pc-windows-gnullvm"; rid = "win-arm64";  ext = ".exe" }
)

foreach ($t in $targets) {
    Log "构建 $($t.name)"
    & cargo zigbuild --release --target $t.name -p snapfind-cli -p snapfind-indexd 2>&1 |
        ForEach-Object { Add-Content -Path $log -Value $_ }
    if ($LASTEXITCODE -ne 0) { Log "失败：$($t.name)（详见日志）"; continue }

    $out = Join-Path $Root "target\$($t.name)\release"
    $dst = Join-Path $Root "dist\$($t.rid)"
    if (-not (Test-Path $dst)) { continue }
    foreach ($bin in @("snapfind", "snapfind-indexd")) {
        $src = Join-Path $out "$bin$($t.ext)"
        if (Test-Path $src) {
            Copy-Item $src (Join-Path $dst "$bin$($t.ext)") -Force
            Log "  → dist\$($t.rid)\$bin$($t.ext)"
        } else {
            Log "  缺失产物：$src"
        }
    }
}

Log "完成"
Get-ChildItem (Join-Path $Root "dist") -Recurse -File |
    ForEach-Object { Log ("{0}\{1} {2}MB" -f $_.Directory.Name, $_.Name, [math]::Round($_.Length / 1MB, 1)) }
