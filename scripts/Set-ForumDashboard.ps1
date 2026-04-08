param(
    [Parameter(Mandatory = $true)]
    [long]$ChatId,

    [switch]$AutoCreateTopics
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$ConfigPath = Join-Path $RepoRoot "telecodex.toml"

if (-not (Test-Path -LiteralPath $ConfigPath)) {
    throw "telecodex config not found: $ConfigPath"
}

$content = Get-Content -LiteralPath $ConfigPath -Raw
$forumLine = "primary_forum_chat_id = $ChatId"
$autoCreateTopicsValue = if ($AutoCreateTopics.IsPresent) { "true" } else { "false" }
$autoLine = "auto_create_topics = $autoCreateTopicsValue"

if ($content -match '(?m)^\[telegram\]\r?\n') {
    if ($content -match '(?m)^\s*primary_forum_chat_id\s*=') {
        $content = [regex]::Replace($content, '(?m)^\s*primary_forum_chat_id\s*=.*$', $forumLine)
    } else {
        $content = [regex]::Replace($content, '(?m)^\[telegram\]\s*$', "[telegram]`r`n$forumLine")
    }

    if ($content -match '(?m)^\s*auto_create_topics\s*=') {
        $content = [regex]::Replace($content, '(?m)^\s*auto_create_topics\s*=.*$', $autoLine)
    } else {
        $content = [regex]::Replace($content, '(?m)^\s*primary_forum_chat_id\s*=.*$', "`$0`r`n$autoLine")
    }
} else {
    throw "telegram section not found in $ConfigPath"
}

Set-Content -LiteralPath $ConfigPath -Value $content -NoNewline
Write-Output "Updated forum dashboard settings in $ConfigPath"
Write-Output "primary_forum_chat_id = $ChatId"
Write-Output "auto_create_topics = $($AutoCreateTopics.IsPresent.ToString().ToLower())"
