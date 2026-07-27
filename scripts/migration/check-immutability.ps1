$ErrorActionPreference = "Stop"

$repoRoot = (git rev-parse --show-toplevel)
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
Set-Location $repoRoot

$migrationDir = Join-Path $repoRoot "crates/tjuaeui-db/migrations"
$duplicateVersions = Get-ChildItem -LiteralPath $migrationDir -File -Filter "*.sql" |
    ForEach-Object {
        if ($_.Name -match '^([0-9]+)_') {
            [PSCustomObject]@{ Version = [int64]$Matches[1]; Name = $_.Name }
        }
    } |
    Group-Object Version |
    Where-Object { $_.Count -gt 1 } |
    Sort-Object Name

if ($duplicateVersions) {
    [Console]::Error.WriteLine("不允许数据库迁移版本号重复。")
    [Console]::Error.WriteLine("")
    [Console]::Error.WriteLine("请将后添加的迁移重命名为下一个未使用的数字前缀。")
    [Console]::Error.WriteLine("")
    [Console]::Error.WriteLine("重复版本：")
    foreach ($duplicate in $duplicateVersions) {
        $names = ($duplicate.Group | ForEach-Object { $_.Name }) -join ", "
        [Console]::Error.WriteLine("$($duplicate.Name): $names")
    }
    exit 1
}

if ($env:TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT -eq "1") {
    Write-Output "TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT=1；已显式允许修改主分支迁移，跳过不可变检查"
    exit 0
}

$baseRef = $env:TJUAECORE_MIGRATION_BASE_REF
if ([string]::IsNullOrWhiteSpace($baseRef)) {
    git rev-parse --verify --quiet origin/main | Out-Null
    if ($LASTEXITCODE -eq 0) {
        $baseRef = "origin/main"
    } else {
        git rev-parse --verify --quiet main | Out-Null
        if ($LASTEXITCODE -eq 0) {
            $baseRef = "main"
        } else {
            Write-Output "未找到 origin/main 或 main 引用，跳过迁移不可变检查"
            exit 0
        }
    }
}

git rev-parse --verify --quiet $baseRef | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Error "未找到迁移不可变检查的基准引用：$baseRef"
    exit 1
}

$baseCommit = git merge-base HEAD $baseRef
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$changed = git diff --name-status --diff-filter=DMR $baseCommit -- "crates/tjuaeui-db/migrations/*.sql"
if (-not [string]::IsNullOrWhiteSpace(($changed -join "`n"))) {
    [Console]::Error.WriteLine("不得修改或删除主分支已有的迁移文件。")
    [Console]::Error.WriteLine("")
    [Console]::Error.WriteLine("请还原对已有迁移文件的修改，并改为添加下一编号的新迁移。")
    [Console]::Error.WriteLine("如果这是有意执行的高风险例外，请设置 TJUAECORE_ALLOW_MAIN_MIGRATION_EDIT=1 后重试。")
    [Console]::Error.WriteLine("")
    [Console]::Error.WriteLine("已变更的现有迁移：")
    [Console]::Error.WriteLine(($changed -join "`n"))
    exit 1
}

Write-Output "迁移不可变检查通过"
