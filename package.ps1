# package.ps1
# 这是一个用于打包 RuaFlashTool 发布版本的 PowerShell 脚本。
# 它将收集所有必要的组件（release exe, scrcpy, platform-tools, Magisk, LKM, ksuinit, KernelPatch, drivers）
# 并将它们组织到一个 'dist' 目录中。

# 定义输出目录和最终自解压包名称
$outputDir = "RuaFlashTool_Release"
$sfxFileName = "RuaFlashTool_Release.exe"

# 清理旧的输出目录和自解压包
Write-Host "开始清理旧的打包文件..." -ForegroundColor Cyan
if (Test-Path $outputDir) {
    Remove-Item $outputDir -Recurse -Force
    Write-Host "旧的 '$outputDir' 目录已清理。" -ForegroundColor Green
}
if (Test-Path $sfxFileName) {
    Remove-Item $sfxFileName -Force
    Write-Host "旧的 '$sfxFileName' 文件已清理。" -ForegroundColor Green
}
New-Item -ItemType Directory -Path $outputDir
Write-Host "清理完成，并创建新的 '$outputDir' 目录。" -ForegroundColor Green

Write-Host "开始编译 RuaFlashTool (Release 模式)..." -ForegroundColor Green
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "错误: RuaFlashTool 编译失败。请检查错误信息。" -ForegroundColor Red
    exit 1
}
Write-Host "编译成功！" -ForegroundColor Green

Write-Host "开始打包 RuaFlashTool..." -ForegroundColor Green

# 1. 复制 Release 可执行文件
Write-Host "复制 rua_cli.exe..." -ForegroundColor Cyan
$releaseExePath = "target/release/rua_cli.exe"
if (Test-Path $releaseExePath) {
    Copy-Item $releaseExePath -Destination $outputDir
    Write-Host "rua_cli.exe 复制成功。" -ForegroundColor Green
} else {
    Write-Host "错误: 未找到 rua_cli.exe。请先运行 'cargo build --release'。" -ForegroundColor Red
    exit 1
}

# 2. 复制 platform-tools 目录
Write-Host "复制 platform-tools 目录..." -ForegroundColor Cyan
$platformToolsPath = "platform-tools"
if (Test-Path $platformToolsPath) {
    Copy-Item $platformToolsPath -Destination $outputDir -Recurse
    Write-Host "platform-tools 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 platform-tools 目录。请确保它存在于项目根目录。" -ForegroundColor Yellow
    # 尝试从可执行文件路径获取
    $exeDir = Split-Path (Get-Location) -Parent
    $altPlatformToolsPath = Join-Path $exeDir "platform-tools"
    if (Test-Path $altPlatformToolsPath) {
        Copy-Item $altPlatformToolsPath -Destination $outputDir -Recurse
        Write-Host "从备用路径复制 platform-tools 成功。" -ForegroundColor Green
    } else {
        Write-Host "错误: 无法找到 platform-tools 目录。" -ForegroundColor Red
        exit 1
    }
}

# 3. 复制 scrcpy 目录 (假设存在于项目根目录)
Write-Host "复制 scrcpy 目录..." -ForegroundColor Cyan
$scrcpyPath = "scrcpy"
if (Test-Path $scrcpyPath) {
    Copy-Item $scrcpyPath -Destination $outputDir -Recurse
    Write-Host "scrcpy 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 scrcpy 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 4. 复制 Magisk 目录 (假设存在于项目根目录)
Write-Host "复制 Magisk 目录..." -ForegroundColor Cyan
$magiskPath = "Magisk"
if (Test-Path $magiskPath) {
    Copy-Item $magiskPath -Destination $outputDir -Recurse
    Write-Host "Magisk 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 Magisk 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 5. 复制 LKM 目录 (假设存在于项目根目录)
Write-Host "复制 LKM 目录..." -ForegroundColor Cyan
$lkmPath = "LKM"
if (Test-Path $lkmPath) {
    Copy-Item $lkmPath -Destination $outputDir -Recurse
    Write-Host "LKM 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 LKM 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 6. 复制 ksuinit 目录 (假设存在于项目根目录)
Write-Host "复制 ksuinit 目录..." -ForegroundColor Cyan
$ksuinitPath = "ksuinit"
if (Test-Path $ksuinitPath) {
    Copy-Item $ksuinitPath -Destination $outputDir -Recurse
    Write-Host "ksuinit 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 ksuinit 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 7. 复制 KernelPatch 目录 (假设存在于项目根目录)
Write-Host "复制 KernelPatch 目录..." -ForegroundColor Cyan
$kernelPatchPath = "KernelPatch"
if (Test-Path $kernelPatchPath) {
    Copy-Item $kernelPatchPath -Destination $outputDir -Recurse
    Write-Host "KernelPatch 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 KernelPatch 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 8. 复制 drivers 目录 (假设存在于项目根目录)
Write-Host "复制 drivers 目录..." -ForegroundColor Cyan
$driversPath = "drivers"
if (Test-Path $driversPath) {
    Copy-Item $driversPath -Destination $outputDir -Recurse
    Write-Host "drivers 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 drivers 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

# 8. 复制 avbkey 目录 (假设存在于项目根目录)
Write-Host "复制 avbkey 目录..." -ForegroundColor Cyan
$avbkeyPath = "avbkey"
if (Test-Path $avbkeyPath) {
    Copy-Item $avbkeyPath -Destination $outputDir -Recurse
    Write-Host "avbkey 复制成功。" -ForegroundColor Green
} else {
    Write-Host "警告: 未找到 avbkey 目录。请确保它存在于项目根目录，否则将不会被打包。" -ForegroundColor Yellow
}

Write-Host "打包完成！发布文件位于 '$outputDir' 目录。" -ForegroundColor Green

# 9. 使用 7-Zip 创建自解压包 (SFX)
Write-Host "开始创建自解压包 (dist.exe)..." -ForegroundColor Green

# 检查 7z.exe 是否存在
$7zPath = Get-Command 7z.exe -ErrorAction SilentlyContinue
if (-not $7zPath) {
    Write-Host "错误: 未找到 7z.exe。请确保 7-Zip 已安装并已添加到系统 PATH 或在脚本中指定其完整路径。" -ForegroundColor Red
    Write-Host "你可以从 https://www.7-zip.org/ 下载 7-Zip。" -ForegroundColor Red
    exit 1
}

# 7-Zip 命令参数
# a: 添加到压缩包
# -t7z: 压缩格式为 7z
# -mx=9: 极限压缩
# -m0=LZMA2: 压缩方法 LZMA2
# -md=128m: 字典大小 128MB
# -mfb=64: 单词大小 64
# -ms=16g: 固实数据大小 16GB
# -mmt=16: CPU 线程数 16
# -sfx: 创建自解压模块
# -r: 递归子目录
# -w: 相对路径 (默认行为，但为了明确可以加上)
# dist.exe: 输出的自解压文件名
# .\dist\*: 要打包的源文件 (dist 目录下的所有内容)

$sfxCommand = "$($7zPath.Source) a -t7z -mx=9 -m0=LZMA2 -md=128m -mfb=64 -ms=16g -mmt=16 -sfx $sfxFileName $($outputDir)\*"


Invoke-Expression $sfxCommand
if ($LASTEXITCODE -ne 0) {
    Write-Host "错误: 创建自解压包失败。请检查 7-Zip 命令输出。" -ForegroundColor Red
    exit 1
}

Write-Host "所有打包任务完成！最终发布文件为 $sfxFileName。" -ForegroundColor Green
