$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).ProviderPath
$script = Join-Path $repoRoot "scripts/migration/check-immutability.ps1"
$tmpdir = Join-Path ([System.IO.Path]::GetTempPath()) ("tjuaecore-migration-test-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tmpdir | Out-Null

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Command,
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]] $Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

function Invoke-WithEnv {
    param(
        [hashtable] $EnvVars,
        [scriptblock] $Body
    )

    $oldValues = @{}
    foreach ($key in $EnvVars.Keys) {
        $oldValues[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
        [Environment]::SetEnvironmentVariable($key, [string]$EnvVars[$key], "Process")
    }

    try {
        & $Body
    } finally {
        foreach ($key in $EnvVars.Keys) {
            [Environment]::SetEnvironmentVariable($key, $oldValues[$key], "Process")
        }
    }
}

function Invoke-InRepo {
    param(
        [string] $Cwd,
        [int] $ExpectedStatus,
        [string] $ExpectedText,
        [hashtable] $EnvVars
    )

    Push-Location $Cwd
    try {
        $output = ""
        Invoke-WithEnv $EnvVars {
            $previousErrorActionPreference = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            try {
                $result = & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $script 2>&1
                $script:actualStatus = $LASTEXITCODE
                $script:actualOutput = ($result | Out-String)
            } finally {
                $ErrorActionPreference = $previousErrorActionPreference
            }
        }
        $output = $script:actualOutput
        $status = $script:actualStatus
    } finally {
        Pop-Location
    }

    if ($status -ne $ExpectedStatus) {
        [Console]::Error.WriteLine("期望状态码 $ExpectedStatus，实际为 $status")
        [Console]::Error.WriteLine($output)
        exit 1
    }

    if (-not [string]::IsNullOrWhiteSpace($ExpectedText) -and -not $output.Contains($ExpectedText)) {
        [Console]::Error.WriteLine("期望输出包含：$ExpectedText")
        [Console]::Error.WriteLine($output)
        exit 1
    }
}

function New-CaseRepo {
    param([string] $Name)

    $dir = Join-Path $tmpdir $Name
    New-Item -ItemType Directory -Force -Path (Join-Path $dir "crates/tjuaeui-db/migrations") | Out-Null

    Push-Location $dir
    try {
        Invoke-Native git init -q -b main
        Invoke-Native git config user.email test@example.com
        Invoke-Native git config user.name "Migration Test"
        Set-Content -LiteralPath "crates/tjuaeui-db/migrations/001_initial_schema.sql" -Value "-- 001 initial"
        Set-Content -LiteralPath "crates/tjuaeui-db/migrations/002_data_fix.sql" -Value "-- 002 data fix"
        Set-Content -LiteralPath "crates/tjuaeui-db/migrations/manual_fixture.sql" -Value "-- auxiliary sql"
        Invoke-Native git add crates/tjuaeui-db/migrations
        Invoke-Native git commit -q -m "seed migrations"
        Invoke-Native git checkout -q -b feature
    } finally {
        Pop-Location
    }

    return $dir
}

try {
    $modifiedRepo = New-CaseRepo "modified"
    Add-Content -LiteralPath (Join-Path $modifiedRepo "crates/tjuaeui-db/migrations/001_initial_schema.sql") -Value "-- modified"
    Invoke-InRepo $modifiedRepo 1 "不得修改或删除主分支已有的迁移文件" @{ TJUAECORE_MIGRATION_BASE_REF = "main" }

    $deletedRepo = New-CaseRepo "deleted"
    Remove-Item -LiteralPath (Join-Path $deletedRepo "crates/tjuaeui-db/migrations/002_data_fix.sql")
    Invoke-InRepo $deletedRepo 1 "不得修改或删除主分支已有的迁移文件" @{ TJUAECORE_MIGRATION_BASE_REF = "main" }

    $auxiliaryRepo = New-CaseRepo "auxiliary"
    Add-Content -LiteralPath (Join-Path $auxiliaryRepo "crates/tjuaeui-db/migrations/manual_fixture.sql") -Value "-- modified auxiliary sql"
    Invoke-InRepo $auxiliaryRepo 1 "不得修改或删除主分支已有的迁移文件" @{ TJUAECORE_MIGRATION_BASE_REF = "main" }

    $addedRepo = New-CaseRepo "added"
    Set-Content -LiteralPath (Join-Path $addedRepo "crates/tjuaeui-db/migrations/003_new_change.sql") -Value "-- 003 new migration"
    Invoke-InRepo $addedRepo 0 "迁移不可变检查通过" @{ TJUAECORE_MIGRATION_BASE_REF = "main" }

    $duplicateRepo = New-CaseRepo "duplicate"
    Set-Content -LiteralPath (Join-Path $duplicateRepo "crates/tjuaeui-db/migrations/002_duplicate_change.sql") -Value "-- duplicate 002 migration"
    Invoke-InRepo $duplicateRepo 1 "不允许数据库迁移版本号重复" @{ TJUAECORE_MIGRATION_BASE_REF = "main" }

    $overrideRepo = New-CaseRepo "override"
    Add-Content -LiteralPath (Join-Path $overrideRepo "crates/tjuaeui-db/migrations/001_initial_schema.sql") -Value "-- modified with explicit override"
    Invoke-InRepo $overrideRepo 0 "跳过不可变检查" @{
        TJUAECORE_MIGRATION_BASE_REF = "main"
        TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT = "1"
    }

    Write-Output "迁移不可变检查脚本测试通过"
} finally {
    Remove-Item -LiteralPath $tmpdir -Recurse -Force
}
