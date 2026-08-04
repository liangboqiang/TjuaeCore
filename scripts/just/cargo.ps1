$ErrorActionPreference = "Stop"

$CargoArgs = @($args)
$cargoConfig = @()
$restoreCargoLock = $false
$cargoLockSnapshot = $null
$tjuae_cliRoot = $null
$crates = @()

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Command,
        [string[]] $Arguments = @()
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        $script:status = $LASTEXITCODE
        exit $LASTEXITCODE
    }
}

function Test-GitDiffClean {
    param([string[]] $Arguments)

    & git @Arguments | Out-Null
    return $LASTEXITCODE -eq 0
}

function Resolve-LocalPath {
    param([string] $Path)

    return [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
}

function Test-TjuaeCliPatch {
    $metadataJson = & cargo @cargoConfig metadata --format-version 1
    if ($LASTEXITCODE -ne 0) {
        $script:status = $LASTEXITCODE
        exit $LASTEXITCODE
    }
    $metadata = $metadataJson | ConvertFrom-Json

    foreach ($crate in $crates) {
        $expectedPath = Resolve-LocalPath (Join-Path $tjuae_cliRoot "crates/$crate")
        $package = $metadata.packages | Where-Object { $_.name -eq $crate } | Select-Object -First 1
        $actualPath = if ($null -eq $package) {
            "未找到包"
        } else {
            Resolve-LocalPath (Split-Path -Parent $package.manifest_path)
        }

        if ($actualPath -ne $expectedPath) {
            Write-Error "$crate 未使用 TJUAE_CLI 补丁。`n  实际解析：$actualPath`n  期望路径：$expectedPath"
            $script:status = 1
            exit 1
        }
    }
}

$status = 0
try {
    if (-not [string]::IsNullOrWhiteSpace($env:TJUAE_CLI)) {
        if (-not (Test-Path -LiteralPath $env:TJUAE_CLI -PathType Container)) {
            Write-Error "TJUAE_CLI 不存在或不是目录：$env:TJUAE_CLI"
            exit 1
        }

        $tjuae_cliRoot = (Resolve-Path -LiteralPath $env:TJUAE_CLI).ProviderPath
        $crates = @(
            "tjuae-agent",
            "tjuae-compact",
            "tjuae-config",
            "tjuae-mcp",
            "tjuae-memory",
            "tjuae-process",
            "tjuae-protocol",
            "tjuae-providers",
            "tjuae-skills",
            "tjuae-tools",
            "tjuae-types"
        )

        foreach ($crate in $crates) {
            $crateDir = Join-Path $tjuae_cliRoot "crates/$crate"
            $manifest = Join-Path $crateDir "Cargo.toml"
            if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
                Write-Error "TJUAE_CLI 缺少 ${crate}：$manifest"
                exit 1
            }

            $tomlPath = $crateDir.Replace("\", "/").Replace('"', '\"')
            $cargoConfig += @("--config", "patch.'https://github.com/liangboqiang/TjuaeCLI.git'.$crate.path = `"`"$tomlPath`"`"")
        }

        [Console]::Error.WriteLine("正在使用本地 TjuaeCLI SDK：$tjuae_cliRoot")

        if (Test-Path -LiteralPath "Cargo.lock" -PathType Leaf) {
            $cargoLockSnapshot = [System.IO.Path]::GetTempFileName()
            Copy-Item -LiteralPath "Cargo.lock" -Destination $cargoLockSnapshot -Force

            $worktreeClean = Test-GitDiffClean @("diff", "--quiet", "--", "Cargo.lock")
            $indexClean = Test-GitDiffClean @("diff", "--cached", "--quiet", "--", "Cargo.lock")
            if ($worktreeClean -and $indexClean) {
                $restoreCargoLock = $true
            } else {
                [Console]::Error.WriteLine("Cargo.lock 已有变更；将保留成功解析 TJUAE_CLI 后的锁文件更新。")
            }
        }

        [Console]::Error.WriteLine("正在针对本地 TjuaeCLI SDK 解析 Cargo.lock")
        $updateArgs = @($cargoConfig) + @(
            "update",
            "-p", "tjuae-agent",
            "-p", "tjuae-compact",
            "-p", "tjuae-config",
            "-p", "tjuae-mcp",
            "-p", "tjuae-memory",
            "-p", "tjuae-process",
            "-p", "tjuae-protocol",
            "-p", "tjuae-providers",
            "-p", "tjuae-skills",
            "-p", "tjuae-tools",
            "-p", "tjuae-types"
        )
        Invoke-Native "cargo" $updateArgs
        Test-TjuaeCliPatch
    }

    & cargo @cargoConfig @CargoArgs
    $status = $LASTEXITCODE
} finally {
    if ($null -ne $cargoLockSnapshot -and (Test-Path -LiteralPath $cargoLockSnapshot -PathType Leaf)) {
        if ($restoreCargoLock -or $status -ne 0) {
            Copy-Item -LiteralPath $cargoLockSnapshot -Destination "Cargo.lock" -Force
        }
        Remove-Item -LiteralPath $cargoLockSnapshot -Force
    }
}

exit $status
