$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Set-Location -LiteralPath $root
New-Item -ItemType Directory -Path package/logs -Force | Out-Null
Start-Transcript -Path package/logs/windows-ci.log -Force

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    Write-Host ('执行：' + $Program + ' ' + ($Arguments -join ' '))
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "命令执行失败：$Program（退出码 $LASTEXITCODE）"
    }
}

try {
    $developerCommand = $env:VSDEVCMD
    if (-not $developerCommand -and $env:RUNNER_ENV_ROOT) {
        $developerCommand = Join-Path $env:RUNNER_ENV_ROOT 'tools/vs2022/Common7/Tools/VsDevCmd.bat'
    }
    if (-not $developerCommand -or -not (Test-Path -LiteralPath $developerCommand -PathType Leaf)) {
        throw '找不到 MSVC 开发环境脚本，请配置 VSDEVCMD 或 RUNNER_ENV_ROOT'
    }
    if ($developerCommand.Contains('"') -or $developerCommand -match '[\r\n]') {
        throw 'MSVC 开发环境脚本路径不合法'
    }
    $commandLine = 'call "' + $developerCommand + '" -arch=x64 -host_arch=x64 >nul && set'
    $developerEnvironment = & $env:ComSpec /d /s /c $commandLine 2>$null
    if ($LASTEXITCODE -ne 0) { throw '导入 MSVC 与 Windows SDK 环境失败' }
    foreach ($entry in $developerEnvironment) {
        if ($entry -match '^([^=][^=]*)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
    $developerEnvironment = $null
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -or -not $env:INCLUDE -or -not $env:LIB) {
        throw 'MSVC 或 Windows SDK 环境不完整'
    }
    # 不使用 Runner 上其他项目的 target，保证产物来源和归档位置明确。
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    # 检查与测试无需调试符号，降低共享 Runner 产物磁盘占用。
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    $env:CARGO_PROFILE_TEST_DEBUG = '0'
    Get-PSDrive -PSProvider FileSystem | Select-Object Name, Used, Free | Format-Table
    # 项目独立安装检查组件，避免修改其他项目共用的 Runner 工具链。
    $env:RUSTUP_HOME = Join-Path $root '.ci-tools/rustup'
    $env:RUSTUP_TOOLCHAIN = $env:RUST_TOOLCHAIN
    if (-not $env:RUSTUP_TOOLCHAIN) { throw '未指定 CI Rust 工具链版本' }
    Invoke-Checked 'rustup' @('toolchain', 'install', $env:RUSTUP_TOOLCHAIN, '--profile', 'minimal', '--component', 'rustfmt', '--component', 'clippy', '--no-self-update')
    Write-Host '已导入 x64 MSVC 环境，开始主站校验与构建'
    Invoke-Checked 'node' @('--version')
    Invoke-Checked 'npm.cmd' @('--version')
    Invoke-Checked 'rustc' @('--version')
    Invoke-Checked 'cargo' @('--version')

    foreach ($lockfile in @('package-lock.json', 'Cargo.lock')) {
        if (-not (Test-Path -LiteralPath $lockfile -PathType Leaf)) {
            if ($env:ALLOW_LOCKFILE_BOOTSTRAP -ne '1') {
                throw "缺少已提交的锁文件：$lockfile"
            }
            Write-Host "初始化锁文件：$lockfile；须从本次 CI 产物收回并提交"
            if ($lockfile -eq 'package-lock.json') {
                Invoke-Checked 'npm.cmd' @('install', '--package-lock-only', '--ignore-scripts', '--no-audit', '--no-fund')
            } else {
                Invoke-Checked 'cargo' @('generate-lockfile')
            }
        }
    }

    if ($env:ALLOW_LOCKFILE_BOOTSTRAP -eq '1') {
        Write-Host '按显式 CI 引导请求刷新 Cargo.lock，完成后须回收提交'
        Invoke-Checked 'cargo' @('generate-lockfile')
    }
    Invoke-Checked 'python' @('-B', '-m', 'unittest', 'discover', '-s', 'scripts/ci', '-p', 'test_*.py', '-v')
    Invoke-Checked 'npm.cmd' @('ci', '--no-audit', '--no-fund')
    # 先生成内嵌前端资源，再检查包含 Tauri 上下文宏的 Rust 桌面工程。
    Invoke-Checked 'npm.cmd' @('run', 'build')
    Invoke-Checked 'cargo' @('fmt', '--all', '--', '--check')
    Invoke-Checked 'cargo' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-Checked 'cargo' @('test', '--workspace', '--locked')
    $signed = $env:CI_COMMIT_BRANCH -eq 'main' -and $env:CI_COMMIT_REF_PROTECTED -eq 'true' -and $env:CI_PIPELINE_SOURCE -in @('push', 'web')
    if ($signed -and -not $env:TAURI_SIGNING_PRIVATE_KEY) { throw '缺少 Tauri 更新签名私钥，停止发布构建' }
    if ($env:CI_PIPELINE_IID -notmatch '^[1-9][0-9]*$') { throw '流水线版本序号无效' }
    $baseVersion = (Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json).version
    $version = "$baseVersion-ci.$($env:CI_PIPELINE_IID)"
    $utf8 = [Text.UTF8Encoding]::new($false)
    $override = @{ version = $version; bundle = @{ createUpdaterArtifacts = $signed } }
    [IO.File]::WriteAllText((Join-Path $root 'package/tauri-ci.json'), ($override | ConvertTo-Json -Depth 5), $utf8)
    # 独立作业目录只删除旧安装包，防止缓存产物被误归档。
    $bundle = Join-Path $env:CARGO_TARGET_DIR 'release/bundle/nsis'
    if (Test-Path -LiteralPath $bundle) {
        Get-ChildItem -LiteralPath $bundle -File | Where-Object { $_.Name -match '\.exe(\.sig)?$' } | Remove-Item
    }
    # 显式传递空密码，避免 PowerShell 删除空值环境变量后签名器等待交互。
    $tauriCommand = 'const {spawnSync}=require("child_process");const r=spawnSync(process.execPath,["node_modules/@tauri-apps/cli/tauri.js",...process.argv.slice(1)],{stdio:"inherit",env:{...process.env,TAURI_SIGNING_PRIVATE_KEY_PASSWORD:process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD??""}});if(r.error)throw r.error;process.exit(r.status??1);'
    Invoke-Checked 'node' @('-e', $tauriCommand, 'build', '--bundles', 'nsis', '--ci', '--config', 'package/tauri-ci.json', '--', '--locked')
    $installers = @(Get-ChildItem -LiteralPath $bundle -File -Filter '*.exe')
    if ($installers.Count -ne 1) { throw '构建结果必须包含一个 Windows NSIS 安装包' }
    New-Item -ItemType Directory -Path package/installer -Force | Out-Null
    Copy-Item -LiteralPath $installers[0].FullName -Destination package/installer/
    if ($signed) {
        $signature = $installers[0].FullName + '.sig'
        if (-not (Test-Path -LiteralPath $signature -PathType Leaf)) { throw '安装包缺少更新签名' }
        Copy-Item -LiteralPath $signature -Destination package/installer/
    }

    $executable = Join-Path $env:CARGO_TARGET_DIR 'release/msk104master.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw 'Tauri 构建完成但找不到主站可执行文件'
    }
    Copy-Item -LiteralPath $executable -Destination package/msk104master.exe
    $utf8 = [Text.UTF8Encoding]::new($false)
    $hash = (Get-FileHash -LiteralPath package/msk104master.exe -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksums = @("$hash  msk104master.exe")
    foreach ($asset in (Get-ChildItem package/installer -File)) {
        $assetHash = (Get-FileHash -LiteralPath $asset.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $checksums += "$assetHash  installer/$($asset.Name)"
    }
    [IO.File]::WriteAllText((Join-Path $root 'package/SHA256SUMS.txt'), (($checksums -join "`n") + "`n"), $utf8)
    $info = [ordered]@{
        version = $version
        signed = $signed
        installer = $installers[0].Name
        commit = $env:CI_COMMIT_SHA
        ref = $env:CI_COMMIT_REF_NAME
        pipeline = $env:CI_PIPELINE_URL
        job = $env:CI_JOB_URL
        target = 'x86_64-pc-windows-msvc'
        built_at = [DateTime]::UtcNow.ToString('o')
        artifact = 'msk104master.exe'
        sha256 = $hash
    }
    [IO.File]::WriteAllText((Join-Path $root 'package/build-info.json'), (($info | ConvertTo-Json) + "`n"), $utf8)
    Write-Host '主站检查与 Windows NSIS 安装包构建完成，受保护 main 的签名产物将进入 R2 发布阶段'
} finally {
    Stop-Transcript
}
