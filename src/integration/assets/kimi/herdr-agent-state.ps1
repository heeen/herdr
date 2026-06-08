# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=kimi
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -notin @("working", "idle", "blocked", "release")) { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

[Console]::In.ReadToEnd() | Out-Null
$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    if ($Action -eq "release") {
        & herdr pane release-agent $env:HERDR_PANE_ID --source herdr:kimi --agent kimi --seq $seq 2>$null | Out-Null
    } else {
        & herdr pane report-agent $env:HERDR_PANE_ID --source herdr:kimi --agent kimi --state $Action --seq $seq 2>$null | Out-Null
    }
} catch {
}
