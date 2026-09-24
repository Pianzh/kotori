# 下载**固定版本**的官方 kopia 二进制并校验官方 checksums.txt,把可执行文件路径
# 写进 KOTORI_KOPIA(通过 $env:GITHUB_ENV,CI 里下一步就能直接用)。verify.yml 的
# Windows 臂靠它养那四个真 kopia 的 ignored 测试;release.yml 的 Windows 打包也
# 用它 —— 同一份逻辑,别在两处各写一份然后漂移。
#
# 用法:
#   powershell ./scripts/install-kopia.ps1 [版本] [安装目录]
#     版本     默认 0.23.1 —— 必须与 release.yml 的 KOPIA_VERSION 对齐
#     安装目录 默认 $PWD/kopia(release.yml 的打包步骤就按这个位置找 kopia.exe / LICENSE)
#
# 输出:
#   * 解压后的 kopia.exe 路径写入 $env:GITHUB_ENV 的 KOTORI_KOPIA(有 GITHUB_ENV 时)
#   * 同时把路径打到 stdout,方便本地手动调试
param(
    [string]$Version = "0.23.1",
    [string]$Dest    = (Join-Path (Get-Location) "kopia")
)

$ErrorActionPreference = 'Stop'

$base  = "https://github.com/kopia/kopia/releases/download/v$Version"
$asset = "kopia-$Version-windows-x64.zip"

Invoke-WebRequest "$base/$asset"        -OutFile kopia.zip
Invoke-WebRequest "$base/checksums.txt" -OutFile kopia-checksums.txt
# 一行一个 "<sha256>  <文件名>"(官方可能写成 "sha *name" 的二进制模式,只取第一段)。
$line = Select-String -Path kopia-checksums.txt -Pattern ([regex]::Escape($asset)) |
        Select-Object -First 1
if (-not $line) { throw "checksums.txt 里没有 $asset" }
$want = ($line.Line -split '\s+')[0].ToLower()
$got  = (Get-FileHash kopia.zip -Algorithm SHA256).Hash.ToLower()
if ($got -ne $want) { throw "kopia 校验失败: 期望 $want, 实际 $got" }
Write-Host "kopia $asset sha256 校验通过"

Expand-Archive kopia.zip -DestinationPath $Dest
Remove-Item kopia.zip, kopia-checksums.txt

# zip 里是一个带版本号的目录,别假设层级,递归找 kopia.exe。
$bin = Get-ChildItem $Dest -Recurse -Filter kopia.exe | Select-Object -First 1
if (-not $bin) { throw "解压出来的包里没有 kopia.exe" }

if ($env:GITHUB_ENV) {
    # runner 上的默认 shell 是 pwsh 7,-Encoding utf8 是无 BOM 的 UTF-8,正确。
    "KOTORI_KOPIA=$($bin.FullName)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
}
Write-Host "KOTORI_KOPIA=$($bin.FullName)"