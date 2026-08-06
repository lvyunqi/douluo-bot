[CmdletBinding()]
param(
    [string]$HostWorktree = $env:QIMENBOT_SOURCE_DIR,
    [string]$PluginDll = "",
    [string]$HostBinary = "",
    [int]$AdminPort = 3221,
    [int]$OneBotPort = 6711,
    [int]$TimeoutSeconds = 30,
    [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"
$AdminToken = "smoke-" + [Guid]::NewGuid().ToString("N")

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "protocol smoke assertion failed: $Message"
    }
}

function Convert-ToTomlPath {
    param([string]$Path)
    return $Path.Replace("\", "/").Replace('"', '\"')
}

function Send-WebSocketText {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [string]$Text)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $segment = [System.ArraySegment[byte]]::new($bytes)
    $Socket.SendAsync(
        $segment,
        [System.Net.WebSockets.WebSocketMessageType]::Text,
        $true,
        [System.Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()
}

function Receive-WebSocketText {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [int]$TimeoutMilliseconds)
    $cts = [System.Threading.CancellationTokenSource]::new($TimeoutMilliseconds)
    $stream = [System.IO.MemoryStream]::new()
    try {
        do {
            $buffer = [byte[]]::new(65536)
            $segment = [System.ArraySegment[byte]]::new($buffer)
            $result = $Socket.ReceiveAsync($segment, $cts.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                return $null
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        return [System.Text.Encoding]::UTF8.GetString($stream.ToArray())
    } finally {
        $cts.Dispose()
        $stream.Dispose()
    }
}

function Get-ActionText {
    param($Action)
    $message = $Action.params.message
    if ($null -eq $message) {
        return ""
    }
    if ($message -is [string]) {
        return $message
    }
    $parts = [System.Collections.Generic.List[string]]::new()
    foreach ($segment in @($message)) {
        if ($segment.type -eq "text") {
            [void]$parts.Add([string]$segment.data.text)
        } elseif ($segment.type -eq "image") {
            [void]$parts.Add("[image:$([string]$segment.data.file)]")
        } elseif ($segment.type -eq "markdown") {
            [void]$parts.Add([string]$segment.data.content)
        }
    }
    return ($parts -join "")
}

function Invoke-OneBotCommand {
    param(
        [string]$Endpoint,
        [string]$Message,
        [string]$UserId = "20001",
        [string]$SelfId = "10001",
        [string]$GroupId = "",
        [hashtable]$ExtraEvent = @{}
    )
    $socket = [System.Net.WebSockets.ClientWebSocket]::new()
    try {
        $socket.ConnectAsync([Uri]$Endpoint, [System.Threading.CancellationToken]::None).GetAwaiter().GetResult()
        $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $lifecycle = [ordered]@{
            time = $now
            self_id = [int64]$SelfId
            post_type = "meta_event"
            meta_event_type = "lifecycle"
            sub_type = "connect"
        }
        Send-WebSocketText $socket ($lifecycle | ConvertTo-Json -Compress -Depth 12)

        $messageId = [int64](Get-Random -Minimum 100000 -Maximum 999999999)
        $event = [ordered]@{
            time = $now
            self_id = [int64]$SelfId
            post_type = "message"
            message_type = $(if ($GroupId) { "group" } else { "private" })
            sub_type = "friend"
            message_id = $messageId
            message_seq = $messageId
            user_id = [int64]$UserId
            message = @(
                [ordered]@{ type = "text"; data = [ordered]@{ text = $Message } }
            )
            message_format = "array"
            raw_message = $Message
            font = 14
            sender = [ordered]@{
                user_id = [int64]$UserId
                nickname = "douluo-protocol-smoke"
                card = ""
                role = "owner"
                title = ""
            }
        }
        if ($GroupId) {
            $event["group_id"] = [int64]$GroupId
        } else {
            $event.sub_type = "friend"
            $event.sender = [ordered]@{
                user_id = [int64]$UserId
                nickname = "douluo-protocol-smoke"
            }
        }
        foreach ($entry in $ExtraEvent.GetEnumerator()) {
            $event[$entry.Key] = $entry.Value
        }
        Send-WebSocketText $socket ($event | ConvertTo-Json -Compress -Depth 20)

        $actions = [System.Collections.Generic.List[object]]::new()
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while ([DateTime]::UtcNow -lt $deadline) {
            $payload = Receive-WebSocketText $socket 10000
            if ([string]::IsNullOrWhiteSpace($payload)) {
                break
            }
            $action = $payload | ConvertFrom-Json
            if ($null -eq $action.action) {
                continue
            }
            [void]$actions.Add($action)
            $actionName = [string]$action.action
            $data = if ($actionName -in @("send_msg", "send_private_msg", "send_group_msg")) {
                [ordered]@{ message_id = [int64](Get-Random -Minimum 900000 -Maximum 999999999) }
            } else {
                [ordered]@{}
            }
            $response = [ordered]@{
                status = "ok"
                retcode = 0
                data = $data
                message = ""
                wording = ""
                echo = $action.echo
            }
            Send-WebSocketText $socket ($response | ConvertTo-Json -Compress -Depth 20)
            # 一次命令的首个 Action 即可证明回调完成；避免等待第二个 Action。
            if ($actions.Count -ge 1) {
                break
            }
        }
        Assert-Condition ($actions.Count -gt 0) "command '$Message' produced no OneBot Action"
        $first = $actions[0]
        [pscustomobject]@{
            Command = $Message
            Action = $first
            ActionName = [string]$first.action
            Text = Get-ActionText $first
            ActionJson = ($first | ConvertTo-Json -Compress -Depth 30)
        }
    } finally {
        if ($socket.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
            # OneBot 宿主收到 Action 回执后可能主动结束短连接；冒烟客户端直接中止即可。
            $socket.Abort()
        }
        $socket.Dispose()
    }
}

function Wait-Health {
    param([string]$Url, [int]$Seconds, [System.Diagnostics.Process]$Process, [string]$ErrorLog)
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Process.HasExited) {
            $tail = if (Test-Path $ErrorLog) { Get-Content -Raw $ErrorLog } else { "" }
            throw "isolated qimenbotd exited with code $($Process.ExitCode): $tail"
        }
        try {
            $response = Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 2
            if ($response.StatusCode -eq 200) {
                return
            }
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    $tail = if (Test-Path $ErrorLog) { Get-Content -Raw $ErrorLog } else { "" }
    throw "isolated qimenbotd did not become healthy at ${Url}: $tail"
}

function Invoke-JsonUtf8 {
    param(
        [string]$Uri,
        [hashtable]$Headers,
        [ValidateSet("Get", "Post")]
        [string]$Method = "Get"
    )
    $response = Invoke-WebRequest -Uri $Uri -Headers $Headers -Method $Method -UseBasicParsing
    $stream = $response.RawContentStream
    $stream.Position = 0
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8)
    try {
        return ($reader.ReadToEnd() | ConvertFrom-Json)
    } finally {
        $reader.Dispose()
    }
}

if ([string]::IsNullOrWhiteSpace($HostWorktree)) {
    throw "provide -HostWorktree or set QIMENBOT_SOURCE_DIR to a QimenBot source checkout"
}
$HostWorktree = (Resolve-Path $HostWorktree).Path
if (-not $PluginDll) {
    $PluginDll = Join-Path $PSScriptRoot "..\target\release\qimen_dynamic_plugin_douluo_game.dll"
}
if (-not $HostBinary) {
    $HostBinary = Join-Path $HostWorktree "target\debug\qimenbotd.exe"
}
$PluginDll = (Resolve-Path $PluginDll).Path
$HostBinary = (Resolve-Path $HostBinary).Path
Assert-Condition (Test-Path $PluginDll) "plugin DLL not found: $PluginDll"
Assert-Condition (Test-Path $HostBinary) "isolated qimenbotd not found: $HostBinary"

$occupied = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Where-Object { $_.LocalPort -in @($AdminPort, $OneBotPort) }
Assert-Condition ($null -eq $occupied) "refusing to use occupied isolated ports $AdminPort/$OneBotPort"

$root = Join-Path ([IO.Path]::GetTempPath()) ("douluo-qimen-smoke-" + [Guid]::NewGuid().ToString("N"))
$bin = Join-Path $root "plugin-bin"
$configDir = Join-Path $root "config"
$pluginConfigDir = Join-Path $configDir "plugins"
$dataDir = Join-Path $root "data"
$logDir = Join-Path $root "logs"
$null = New-Item -ItemType Directory -Force -Path $bin, $pluginConfigDir, $dataDir, $logDir
Copy-Item -LiteralPath $PluginDll -Destination (Join-Path $bin "qimen_dynamic_plugin_douluo_game.dll")

$tomlBin = Convert-ToTomlPath $bin
$tomlConfigDir = Convert-ToTomlPath $pluginConfigDir
$tomlState = Convert-ToTomlPath (Join-Path $configDir "plugin-state.toml")
$tomlAudit = Convert-ToTomlPath (Join-Path $configDir "admin-audit.jsonl")
$config = @"
[runtime]
env = "dev"
shutdown_timeout_secs = 15
task_grace_secs = 5

[observability]
level = "debug"
json_logs = false
metrics_bind = "127.0.0.1:0"

[admin_web]
enabled = true
bind = "127.0.0.1:$AdminPort"
access_token = "$AdminToken"
log_capacity = 2000
audit_path = "$tomlAudit"

[marketplace]
enabled = false
cache_dir = "cache/marketplace"
lock_path = "config/marketplace-lock.toml"
request_timeout_secs = 30
allow_prerelease = false
auto_update = false

[official_host]
builtin_modules = ["command"]
plugin_modules = []
plugin_state_path = "$tomlState"
plugin_bin_dir = "$tomlBin"
plugin_config_dir = "$tomlConfigDir"
dynamic_plugin_timeout_secs = 30

[official_host.commands]
help_enabled = true
help_page_size = 8
plugins_enabled = true
registry_enabled = true
dynamic_errors_enabled = true
prefixes = ["/"]
private_bare_enabled = true
group_bare_enabled = true
mention_enabled = true
reply_enabled = true

[official_host.proactive_send]
queue_capacity = 32
offline_ttl_secs = 0

[official_host.webhook]
enabled = false
bind = "127.0.0.1:0"
base_path = "/webhooks"
max_body_bytes = 1048576
request_timeout_ms = 5000
max_in_flight = 8
access_token = ""

[[bots]]
id = "douluo-smoke"
account_id = "10001"
protocol = "onebot11"
transport = "ws-reverse"
bind = "127.0.0.1:$OneBotPort"
path = "/onebot/smoke"
enabled = true
enabled_modules = ["command"]
owners = ["20001"]
admins = []
auto_reply_poke_enabled = false
"@
Set-Content -LiteralPath (Join-Path $configDir "base.toml") -Value $config -Encoding UTF8
$pluginState = @'
[modules]
"douluo-game" = true
'@
Set-Content -LiteralPath (Join-Path $configDir "plugin-state.toml") -Value $pluginState -Encoding UTF8
$pluginConfig = @"
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000
[identity]
namespace = "smoke"
[authorization]
mode = "allow_all"
[illustrations]
enabled = false
mode = "direct"
[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
"@
Set-Content -LiteralPath (Join-Path $pluginConfigDir "douluo-game.toml") -Value $pluginConfig -Encoding UTF8

$stdoutLog = Join-Path $logDir "qimenbotd.stdout.log"
$stderrLog = Join-Path $logDir "qimenbotd.stderr.log"
$oldConfigPath = $env:QIMEN_CONFIG_PATH
$env:QIMEN_CONFIG_PATH = "config/base.toml"
$process = $null
try {
    $process = Start-Process -FilePath $HostBinary -WorkingDirectory $root -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -PassThru
} finally {
    if ($null -eq $oldConfigPath) { Remove-Item Env:QIMEN_CONFIG_PATH -ErrorAction SilentlyContinue }
    else { $env:QIMEN_CONFIG_PATH = $oldConfigPath }
}

try {
    Wait-Health "http://127.0.0.1:$AdminPort/healthz" $TimeoutSeconds $process $stderrLog
    $headers = @{ Authorization = "Bearer $AdminToken" }
    $plugins = Invoke-JsonUtf8 "http://127.0.0.1:$AdminPort/api/v1/plugins" $headers Get
    $plugin = @($plugins.data) | Where-Object { $_.id -eq "douluo-game" } | Select-Object -First 1
    Assert-Condition ($null -ne $plugin) "dynamic plugin descriptor is missing"
    Assert-Condition ([bool]$plugin.loaded) "dynamic plugin descriptor exists but init did not complete"
    # PowerShell 5 can wrap a JSON array in a single PSObject; normalize each
    # command explicitly before comparing Unicode command names.
    $commands = @($plugin.commands | ForEach-Object { [string]$_ })
    foreach ($command in @(
        "斗罗系统", "开始穿越", "武魂觉醒", "签到", "钱包", "状态", "位置",
        "地图列表", "向", "传送", "NPC", "对话", "商店", "背包", "购买", "出售", "使用"
    )) {
        $found = $commands | Where-Object { $_ -eq $command }
        Assert-Condition ($null -ne $found) "descriptor is missing command '$command' (commands=$($commands -join ', '))"
    }
    Write-Output "descriptor: douluo-game and expected command set loaded"

    $endpoint = "ws://127.0.0.1:$OneBotPort/onebot/smoke"
    $checks = @(
        @{ Message = "斗罗系统"; Contains = "欢迎来到斗罗大陆" },
        @{ Message = "开始穿越 协议冒烟 男"; Contains = "穿越成功" },
        @{ Message = "武魂觉醒"; Contains = "觉醒仪式完成" },
        @{ Message = "签到"; Contains = "签到成功" },
        @{ Message = "签到"; Contains = "今日已签到" },
        @{ Message = "钱包"; Contains = "金魂币" },
        @{ Message = "位置"; Contains = "圣魂村" },
        @{ Message = "余额"; Contains = "金魂币" },
        @{ Message = "NPC"; Contains = "杂货商人" },
        @{ Message = "对话 杂货商人"; Contains = "当前对话已绑定" },
        @{ Message = "商店"; Contains = "小回复药" },
        @{ Message = "购买 小回复药 2"; Contains = "购买成功" },
        @{ Message = "背包"; Contains = "小回复药 x2" },
        @{ Message = "使用 小回复药"; Contains = "物品未消耗" },
        @{ Message = "出售 小回复药-1"; Contains = "出售成功" }
    )
    foreach ($check in $checks) {
        $result = Invoke-OneBotCommand $endpoint $check.Message
        Write-Output ("onebot {0}: {1} [{2}]" -f $result.Command, $result.ActionName, $result.Text)
        Assert-Condition ($result.Text.Contains($check.Contains)) "response for '$($check.Message)' did not contain '$($check.Contains)': $($result.ActionJson)"
    }
    $group = Invoke-OneBotCommand $endpoint "位置" -GroupId "40001"
    Write-Output ("onebot group 位置: {0} [{1}]" -f $group.ActionName, $group.Text)
    Assert-Condition ($group.Text.Contains("圣魂村")) "group command did not respond"

    # 仅验证插件的 QQ 协议识别/Markdown 内容生成；OneBot 传输不会模拟 QQ Gateway 投递。
    $qqPayload = [ordered]@{
        event_type = "C2C_MESSAGE_CREATE"
        id = "synthetic-qq-message"
        author = [ordered]@{ user_openid = "synthetic-openid" }
    }
    $qq = Invoke-OneBotCommand $endpoint "斗罗系统" -UserId "30001" -ExtraEvent @{ qqbot_payload = $qqPayload }
    Write-Output ("synthetic qq payload: {0} [{1}]" -f $qq.ActionName, $qq.Text)
    Assert-Condition ($qq.Text.Contains("欢迎来到斗罗大陆")) "synthetic qq payload lost text fallback"

    $reload = Invoke-JsonUtf8 "http://127.0.0.1:$AdminPort/api/v1/plugins/reload" $headers Post
    $reloadJson = $reload | ConvertTo-Json -Depth 30 -Compress
    Write-Output "reload: $reloadJson"
    Assert-Condition ($null -ne $reload.data.message) "dynamic reload did not return a result"
    $afterReload = Invoke-OneBotCommand $endpoint "钱包"
    Assert-Condition ($afterReload.Text.Contains("金魂币")) "command failed after dynamic reload"
    Write-Output "protocol smoke passed: OneBot private/group, synthetic QQ payload, descriptor, and reload"
} finally {
    if ($null -ne $process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        [void]$process.WaitForExit(5000)
    }
    if (-not $KeepTemp) {
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    } else {
        Write-Output "kept temporary smoke root: $root"
    }
}
