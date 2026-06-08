# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=copilot
# HERDR_INTEGRATION_VERSION=1

if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { @{} } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = @{}
}

function First-Text {
    param([object[]]$Names)
    foreach ($name in $Names) {
        $value = $payload.$name
        if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) {
            return $value
        }
    }
    return $null
}

function Normalize-Event {
    param([string]$Event)
    if ([string]::IsNullOrWhiteSpace($Event)) { return "" }
    return $Event.Replace("_", "").Replace("-", "").ToLowerInvariant()
}

function Infer-Event {
    $explicit = First-Text @("hook_event_name", "hookEventName")
    if ($explicit) { return $explicit }
    if (First-Text @("notification_type", "notificationType")) { return "notification" }
    if ($payload.PSObject.Properties.Name -contains "toolResult" -or $payload.PSObject.Properties.Name -contains "tool_result") { return "postToolUse" }
    if (($payload.PSObject.Properties.Name -contains "error") -and (First-Text @("tool_name", "toolName"))) { return "postToolUseFailure" }
    if (First-Text @("tool_name", "toolName")) { return "preToolUse" }
    if (First-Text @("stop_reason", "stopReason")) { return "agentStop" }
    if (First-Text @("reason")) { return "sessionEnd" }
    if ($payload.PSObject.Properties.Name -contains "prompt") { return "userPromptSubmitted" }
    if (
        $payload.PSObject.Properties.Name -contains "initial_prompt" -or
        $payload.PSObject.Properties.Name -contains "initialPrompt" -or
        $payload.PSObject.Properties.Name -contains "source" -or
        (First-Text @("session_id", "sessionId"))
    ) { return "sessionStart" }
    return ""
}

function Has-Initial-Prompt {
    $value = $payload.initial_prompt
    if ($null -eq $value) { $value = $payload.initialPrompt }
    return ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value))
}

function Report-State {
    param([string]$State, [string]$SessionId)
    $seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $args = @("pane", "report-agent", $env:HERDR_PANE_ID, "--source", "herdr:copilot", "--agent", "copilot", "--state", $State, "--seq", "$seq")
    if (-not [string]::IsNullOrWhiteSpace($SessionId)) {
        $args += @("--agent-session-id", $SessionId)
    }
    & herdr @args 2>$null | Out-Null
}

function Release-Agent {
    $seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    & herdr pane release-agent $env:HERDR_PANE_ID --source herdr:copilot --agent copilot --seq $seq 2>$null | Out-Null
}

try {
    $eventKey = Normalize-Event (Infer-Event)
    $sessionId = First-Text @("session_id", "sessionId")
    $toolName = First-Text @("tool_name", "toolName")
    $notificationType = First-Text @("notification_type", "notificationType")
    $stopReason = First-Text @("stop_reason", "stopReason")
    $reason = First-Text @("reason")

    if ($eventKey -eq "sessionstart") {
        Report-State ($(if (Has-Initial-Prompt) { "working" } else { "idle" })) $sessionId
    } elseif ($eventKey -in @("userpromptsubmit", "userpromptsubmitted")) {
        Report-State "working" $sessionId
    } elseif ($eventKey -eq "pretooluse") {
        Report-State ($(if ($toolName -in @("ask_user", "exit_plan_mode")) { "blocked" } else { "working" })) $sessionId
    } elseif ($eventKey -in @("posttooluse", "posttoolusefailure")) {
        if ($toolName -ne "report_intent") { Report-State "working" $sessionId }
    } elseif ($eventKey -eq "notification") {
        if ($notificationType -in @("permission_prompt", "elicitation_dialog")) {
            Report-State "blocked" $sessionId
        } elseif ($notificationType -eq "agent_idle") {
            Report-State "idle" $sessionId
        }
    } elseif ($eventKey -in @("stop", "agentstop", "sessionstop")) {
        if ([string]::IsNullOrWhiteSpace($stopReason) -or $stopReason -eq "end_turn") {
            Report-State "idle" $sessionId
        }
    } elseif ($eventKey -eq "sessionend") {
        if ($reason -in @("user_exit", "abort")) { Release-Agent }
    }
} catch {
}
