param([ValidateSet('build', 'cache-finish')][string]$Stage = 'build')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Set-Location -LiteralPath $root
New-Item -ItemType Directory -Path package/logs -Force | Out-Null
$utf8 = [Text.UTF8Encoding]::new($false)

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    Write-Host ('执行：' + $Program + ' ' + ($Arguments -join ' '))
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "命令执行失败：$Program（退出码 $LASTEXITCODE）"
    }
}

function Invoke-Cache([string]$Action, [string]$ReportName, [string[]]$ExtraArguments = @()) {
    Write-Host "主站编译缓存操作：$Action"
    $cacheOutput = & python -B scripts/ci/cargo_cache.py $Action --root $root @ExtraArguments
    $cacheExit = $LASTEXITCODE
    $reportPath = Join-Path $root "package/logs/cargo-cache-$ReportName.json"
    [IO.File]::WriteAllText($reportPath, (($cacheOutput -join "`n") + "`n"), $utf8)
    Write-Host ($cacheOutput -join "`n")
    if ($cacheExit -ne 0) { throw "主站编译缓存操作失败：$Action（退出码 $cacheExit），详见 $reportPath" }
}

function Invoke-CargoStep([string]$Program, [string[]]$Arguments, [string]$CacheStage) {
    Invoke-Cache 'check' "before-$CacheStage"
    $logPath = Join-Path $root "package/logs/$CacheStage-cargo.jsonl"
    $commandFailure = $null
    $recordFailure = $null
    $commandExit = $null
    Write-Host ('执行：' + $Program + ' ' + ($Arguments -join ' '))
    try {
        & $Program @Arguments | Tee-Object -FilePath $logPath
        $commandExit = $LASTEXITCODE
    } catch {
        $commandFailure = $_
    }
    # 即使编译失败也记录已产生的准确路径，保留主命令失败为最终原因。
    try {
        Invoke-Cache 'record' "record-$CacheStage" @('--stage', $CacheStage, '--log', $logPath)
    } catch {
        $recordFailure = $_
    }
    if ($commandFailure) { throw $commandFailure }
    if ($commandExit -ne 0) { throw "命令执行失败：$Program（退出码 $commandExit）" }
    if ($recordFailure) { throw $recordFailure }
}

function Restore-CargoRecords {
    # 取消可能打断编译后的登记，只重放当前作业目录中的固定日志。
    foreach ($cacheStage in @('clippy', 'test', 'package')) {
        $logPath = Join-Path $root "package/logs/$cacheStage-cargo.jsonl"
        if (-not (Test-Path -LiteralPath $logPath -PathType Leaf)) { continue }
        try {
            Invoke-Cache 'record' "recover-$cacheStage" @('--stage', $cacheStage, '--log', $logPath)
        } catch {
            Write-Warning "恢复主站 $cacheStage 缓存登记失败，继续其他阶段与失败收尾：$($_.Exception.Message)"
        }
    }
}

if ($Stage -eq 'cache-finish') {
    # after_script 使用新 shell；成功作业已经收尾，不得改写成功记录。
    if ($env:CI_JOB_STATUS -eq 'success') { return }
    Restore-CargoRecords
    Invoke-Cache 'finish' 'after-script' @('--status', 'failed')
    return
}

Start-Transcript -Path package/logs/windows-ci.log -Force
try {
    # 先在 CI 验证缓存边界，再对本项目的持久缓存执行准备和空间检查。
    Invoke-Checked 'python' @('-B', '-m', 'unittest', 'discover', '-s', 'scripts/ci', '-p', 'test_*.py', '-v')
    Invoke-Cache 'prepare' 'prepare'
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
    # 检查、测试和打包统一 Release；环境导入后再次固定参数。
    $env:CARGO_INCREMENTAL = '0'
    $env:CARGO_PROFILE_RELEASE_DEBUG = '0'
    $env:CARGO_PROFILE_RELEASE_OPT_LEVEL = '3'
    $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '16'
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
    Invoke-Cache 'check' 'before-frontend'
    Invoke-Checked 'npm.cmd' @('ci', '--no-audit', '--no-fund')
    # 先生成内嵌前端资源，再检查包含 Tauri 上下文宏的 Rust 桌面工程。
    Invoke-Checked 'npm.cmd' @('run', 'build')
    Invoke-Checked 'cargo' @('fmt', '--all', '--', '--check')
    Invoke-CargoStep 'cargo' @('clippy', '--release', '--workspace', '--all-targets', '--features', 'msk104master/custom-protocol', '--target', 'x86_64-pc-windows-msvc', '--locked', '--message-format=json-render-diagnostics', '--', '-D', 'warnings') 'clippy'
    Invoke-CargoStep 'cargo' @('test', '--release', '--workspace', '--features', 'msk104master/custom-protocol', '--target', 'x86_64-pc-windows-msvc', '--locked', '--message-format=json-render-diagnostics') 'test'
    $signed = $env:CI_COMMIT_BRANCH -eq 'main' -and $env:CI_COMMIT_REF_PROTECTED -eq 'true' -and $env:CI_PIPELINE_SOURCE -in @('push', 'web')
    if ($signed -and -not $env:TAURI_SIGNING_PRIVATE_KEY) { throw '缺少 Tauri 更新签名私钥，停止发布构建' }
    if ($env:CI_PIPELINE_IID -notmatch '^[1-9][0-9]*$') { throw '流水线版本序号无效' }
    $baseVersion = (Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json).version
    $version = "$baseVersion-ci.$($env:CI_PIPELINE_IID)"
    $utf8 = [Text.UTF8Encoding]::new($false)
    $override = @{ version = $version; bundle = @{ createUpdaterArtifacts = $signed } }
    [IO.File]::WriteAllText((Join-Path $root 'package/tauri-ci.json'), ($override | ConvertTo-Json -Depth 5), $utf8)
    # 独立作业目录只删除旧安装包，防止缓存产物被误归档。
    $bundle = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release/bundle/nsis'
    if (Test-Path -LiteralPath $bundle) {
        Get-ChildItem -LiteralPath $bundle -File | Where-Object { $_.Name -match '\.exe(\.sig)?$' } | Remove-Item
    }
    # 显式传递空密码，避免 PowerShell 删除空值环境变量后签名器等待交互。
    $tauriCommand = 'const {spawnSync}=require("child_process");const r=spawnSync(process.execPath,["node_modules/@tauri-apps/cli/tauri.js",...process.argv.slice(1)],{stdio:"inherit",env:{...process.env,TAURI_SIGNING_PRIVATE_KEY_PASSWORD:process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD??""}});if(r.error)throw r.error;process.exit(r.status??1);'
    Invoke-CargoStep 'node' @('-e', $tauriCommand, 'build', '--bundles', 'nsis', '--features', 'custom-protocol', '--target', 'x86_64-pc-windows-msvc', '--ci', '--config', 'package/tauri-ci.json', '--', '--locked', '--message-format=json-render-diagnostics') 'package'
    $installers = @(Get-ChildItem -LiteralPath $bundle -File -Filter '*.exe')
    if ($installers.Count -ne 1) { throw '构建结果必须包含一个 Windows NSIS 安装包' }
    New-Item -ItemType Directory -Path package/installer -Force | Out-Null
    Copy-Item -LiteralPath $installers[0].FullName -Destination package/installer/
    if ($signed) {
        $signature = $installers[0].FullName + '.sig'
        if (-not (Test-Path -LiteralPath $signature -PathType Leaf)) { throw '安装包缺少更新签名' }
        Copy-Item -LiteralPath $signature -Destination package/installer/
    }

    $executable = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release/msk104master.exe'
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
    # 预算检查失败不能推进成功历史；全部产物归档成功后才更新保留集。
    Invoke-Cache 'check' 'after-build'
    Invoke-Cache 'finish' 'success' @('--status', 'success')
    Write-Host '主站检查与 Windows NSIS 安装包构建完成，受保护 main 的签名产物将进入 R2 发布阶段'
} catch {
    $buildFailure = $_
    try {
        Restore-CargoRecords
        Invoke-Cache 'finish' 'failed' @('--status', 'failed')
    } catch {
        Write-Warning "主站失败缓存收尾未完成：$($_.Exception.Message)"
    }
    throw $buildFailure
} finally {
    Stop-Transcript
}
