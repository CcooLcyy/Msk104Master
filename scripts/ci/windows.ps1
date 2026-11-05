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

    Invoke-Checked 'npm.cmd' @('ci', '--no-audit', '--no-fund')
    # 先生成内嵌前端资源，再检查包含 Tauri 上下文宏的 Rust 桌面工程。
    Invoke-Checked 'npm.cmd' @('run', 'build')
    Invoke-Checked 'cargo' @('fmt', '--all', '--', '--check')
    Invoke-Checked 'cargo' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-Checked 'cargo' @('test', '--workspace', '--locked')
    Invoke-Checked 'npm.cmd' @('run', 'tauri', '--', 'build', '--no-bundle', '--ci', '--', '--locked')

    $executable = Join-Path $env:CARGO_TARGET_DIR 'release/msk104master.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw 'Tauri 构建完成但找不到主站可执行文件'
    }
    Copy-Item -LiteralPath $executable -Destination package/msk104master.exe
    $utf8 = [Text.UTF8Encoding]::new($false)
    $hash = (Get-FileHash -LiteralPath package/msk104master.exe -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $root 'package/SHA256SUMS.txt'), "$hash  msk104master.exe`n", $utf8)
    $info = [ordered]@{
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
    Write-Host '主站检查与 Windows 可执行文件构建完成；产物仅归档，不发布更新渠道'
} finally {
    Stop-Transcript
}
