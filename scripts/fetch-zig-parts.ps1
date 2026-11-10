# 多连接分段下载 zig 压缩包的剩余部分（ziglang.org 单连接限速严重）
# 产物：.tools\p0 .. p{n-1}，需按序追加到已有 .tools\zig.zip 之后
param(
    [string]$Root = "F:\Code\CodeBuddy\SnapFind",
    [int]$Segments = 6,
    [long]$Total = 100343666,
    [string]$Url = "https://ziglang.org/download/0.17.1/zig-x86_64-windows-0.17.1.zip"
)

Set-Location $Root
$base = (Get-Item ".tools\zig.zip").Length
$remaining = $Total - $base
if ($remaining -le 0) { Write-Host "already complete"; exit 0 }

$seg = [math]::Ceiling($remaining / $Segments)
for ($i = 0; $i -lt $Segments; $i++) {
    $start = $base + $i * $seg
    $end = [math]::Min($start + $seg - 1, $Total - 1)
    $args = @("-sS", "-r", "$start-$end", "-o", ".tools\p$i", "--retry", "10", "--retry-delay", "2", $Url)
    Start-Process -FilePath "curl.exe" -ArgumentList $args -WindowStyle Hidden
}
Write-Host "launched $Segments segments from $base (seg=$seg)"
