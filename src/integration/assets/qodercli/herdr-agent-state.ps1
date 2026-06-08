# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=qodercli
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -notin @("working", "idle", "blocked", "release")) { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

if ($payload.hook_event_name -eq "SubagentStop") { exit 0 }
if ($payload.agent_id -and $Action -in @("idle", "release")) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    if ($Action -eq "release") {
        & herdr pane release-agent $env:HERDR_PANE_ID --source herdr:qodercli --agent qodercli --seq $seq 2>$null | Out-Null
    } else {
        $args = @("pane", "report-agent", $env:HERDR_PANE_ID, "--source", "herdr:qodercli", "--agent", "qodercli", "--state", $Action, "--seq", "$seq")
        if (-not [string]::IsNullOrWhiteSpace($payload.session_id)) {
            $args += @("--agent-session-id", $payload.session_id)
        }
        & herdr @args 2>$null | Out-Null
    }
} catch {
}
