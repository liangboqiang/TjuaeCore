$ErrorActionPreference = "Stop"

$configFile = if ([string]::IsNullOrWhiteSpace($env:TJUAE_CONFIG_DEV_FILE)) {
    Join-Path $HOME ".tjuaeui-config-dev/tjuaeui-config.txt"
} else {
    $env:TJUAE_CONFIG_DEV_FILE
}

if (-not (Test-Path -LiteralPath $configFile -PathType Leaf)) {
    Write-Error "未找到配置文件：$configFile"
    exit 1
}

$encoded = (Get-Content -LiteralPath $configFile -Raw).Trim()
$bytes = [Convert]::FromBase64String($encoded)
$decoded = [Text.Encoding]::UTF8.GetString($bytes)
$plain = [Uri]::UnescapeDataString($decoded)

if (Get-Command Set-Clipboard -ErrorAction SilentlyContinue) {
    Set-Clipboard -Value $plain
    Write-Output "配置已复制到剪贴板"
} else {
    Write-Output $plain
}
