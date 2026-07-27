$ErrorActionPreference = "Stop"

$repoRoot = (git rev-parse --show-toplevel)
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
Set-Location $repoRoot

# 拆分字面量，避免门禁脚本把自身识别为违规内容。
$forbiddenLiterals = @(
    ("ai" + "on"),
    ("ioffice" + "ai"),
    ("office" + "cli"),
    ("sen" + "try"),
    ("tele" + "metry"),
    ("molt" + "book"),
    ("openclaw" + "-setup"),
    ("yolo" + "nosandbox")
)
$escapedLiterals = $forbiddenLiterals | ForEach-Object { [Regex]::Escape($_) }
$forbiddenPattern = (($escapedLiterals + "morph[ -]?ppt") -join "|")

$contentMatches = & git grep -I -n -i -E $forbiddenPattern -- . ":(exclude)UPSTREAM.md"
$contentStatus = $LASTEXITCODE
if ($contentStatus -gt 1) {
    exit $contentStatus
}

$pathMatches = @(
    git ls-files |
        Where-Object {
            $_ -ne "UPSTREAM.md" -and $_ -match $forbiddenPattern
        }
)

if ($contentStatus -eq 0 -or $pathMatches.Count -gt 0) {
    [Console]::Error.WriteLine("检测到禁止的旧品牌、黑盒能力或外部推广内容。")
    if ($contentStatus -eq 0) {
        [Console]::Error.WriteLine("")
        [Console]::Error.WriteLine("内容命中：")
        [Console]::Error.WriteLine(($contentMatches -join "`n"))
    }
    if ($pathMatches.Count -gt 0) {
        [Console]::Error.WriteLine("")
        [Console]::Error.WriteLine("路径命中：")
        [Console]::Error.WriteLine(($pathMatches -join "`n"))
    }
    exit 1
}

Write-Output "身份与去推广门禁通过"
