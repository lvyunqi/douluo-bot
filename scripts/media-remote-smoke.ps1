[CmdletBinding()]
param(
    [string]$HostWorktree = $env:QIMENBOT_SOURCE_DIR,
    [string]$PluginDll = "",
    [string]$HostBinary = "",
    [string]$MediaBinary = "",
    [string]$CaddyBinary = "",
    [int]$AdminPort = 0,
    [int]$OneBotPort = 0,
    [int]$MediaPort = 0,
    [int]$ProxyPort = 0,
    [int]$TimeoutSeconds = 30,
    [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"
$TestHost = "media.example.test"
$BasePath = "/douluo"
$AssetKey = "maps/holy-soul-village/cover.webp"

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "media remote smoke assertion failed: $Message"
    }
}

function Get-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        0
    )
    $listener.Start()
    try {
        return $listener.LocalEndpoint.Port
    } finally {
        $listener.Stop()
    }
}

function Resolve-RequiredPath {
    param([string]$Path, [string]$Description)
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($Path)) "missing $Description"
    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction SilentlyContinue
    Assert-Condition ($null -ne $resolved) "$Description not found: $Path"
    return $resolved.Path
}

function Join-ProcessArguments {
    param([string[]]$Arguments)
    return (($Arguments | ForEach-Object {
        $argument = [string]$_
        if ($argument -match '[\s"]') {
            '"' + $argument.Replace('"', '\"') + '"'
        } else {
            $argument
        }
    }) -join " ")
}

# 仅在子进程启动窗口注入环境变量，避免污染后续宿主或测试进程。
function Start-WithEnvironment {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [hashtable]$Environment,
        [string]$StdoutPath,
        [string]$StderrPath
    )
    $saved = @{}
    try {
        foreach ($key in $Environment.Keys) {
            $saved[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
            [Environment]::SetEnvironmentVariable($key, [string]$Environment[$key], "Process")
        }
        $startParameters = @{
            FilePath = $FilePath
            WorkingDirectory = $WorkingDirectory
            RedirectStandardOutput = $StdoutPath
            RedirectStandardError = $StderrPath
            WindowStyle = "Hidden"
            PassThru = $true
        }
        $argumentString = Join-ProcessArguments $Arguments
        if (-not [string]::IsNullOrWhiteSpace($argumentString)) {
            $startParameters.ArgumentList = $argumentString
        }
        return Start-Process @startParameters
    } finally {
        foreach ($key in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($key, $saved[$key], "Process")
        }
    }
}

function Stop-ProcessIfRunning {
    param([System.Diagnostics.Process]$Process)
    if ($null -ne $Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
        [void]$Process.WaitForExit(5000)
    }
}

function Get-LogTail {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return ""
    }
    $lines = @(Get-Content -LiteralPath $Path)
    if ($lines.Count -le 20) {
        return ($lines -join [Environment]::NewLine)
    }
    return ($lines[($lines.Count - 20)..($lines.Count - 1)] -join [Environment]::NewLine)
}

# 将响应头和原始字节分别落入临时文件，供 HTTPS 与字节一致性断言复用。
function Invoke-CurlResponse {
    param(
        [string]$Uri,
        [ValidateSet("GET", "HEAD", "POST")]
        [string]$Method = "GET",
        [string]$HostHeader,
        [string]$WorkDirectory,
        [string]$ResolveAddress
    )
    $id = [Guid]::NewGuid().ToString("N")
    $headerPath = Join-Path $WorkDirectory "$id.headers"
    $bodyPath = Join-Path $WorkDirectory "$id.body"
    $curlErrorPath = Join-Path $WorkDirectory "$id.curl.stderr"
    $arguments = @(
        "--silent", "--show-error", "--insecure", "--path-as-is",
        "--max-time", "5", "--request", $Method,
        "--dump-header", $headerPath, "--output", $bodyPath,
        "--write-out", "%{http_code}", $Uri
    )
    if (-not [string]::IsNullOrWhiteSpace($HostHeader)) {
        $arguments = @("--header", "Host: $HostHeader") + $arguments
    }
    if (-not [string]::IsNullOrWhiteSpace($ResolveAddress)) {
        $parsedUri = [Uri]$Uri
        $arguments = @("--resolve", "$($parsedUri.Host):$($parsedUri.Port):$ResolveAddress") + $arguments
    }
    $statusText = [string](& curl.exe @arguments 2> $curlErrorPath)
    if ($LASTEXITCODE -ne 0) {
        $curlError = if (Test-Path -LiteralPath $curlErrorPath) {
            [string](Get-Content -LiteralPath $curlErrorPath -Raw)
        } else {
            ""
        }
        throw "curl failed for $Method ${Uri}: $statusText $curlError"
    }
    $status = 0
    Assert-Condition ([int]::TryParse($statusText.Trim(), [ref]$status)) "curl returned no HTTP status for $Uri"
    $headerLines = @(Get-Content -LiteralPath $headerPath)
    $start = 0
    for ($index = 0; $index -lt $headerLines.Count; $index++) {
        if ($headerLines[$index] -match '^HTTP/') {
            $start = $index
        }
    }
    $headers = @{}
    for ($index = $start + 1; $index -lt $headerLines.Count; $index++) {
        $line = [string]$headerLines[$index]
        if ([string]::IsNullOrWhiteSpace($line)) {
            break
        }
        $separator = $line.IndexOf(':')
        if ($separator -gt 0) {
            $name = $line.Substring(0, $separator).Trim()
            $value = $line.Substring($separator + 1).Trim()
            $headers[$name] = $value
        }
    }
    [pscustomobject]@{
        StatusCode = $status
        Headers = $headers
        BodyBytes = if (Test-Path -LiteralPath $bodyPath) { [IO.File]::ReadAllBytes($bodyPath) } else { [byte[]]@() }
        BodyText = if (Test-Path -LiteralPath $bodyPath) { [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($bodyPath)) } else { "" }
    }
}

function Get-HeaderValue {
    param($Response, [string]$Name)
    foreach ($key in $Response.Headers.Keys) {
        if ([string]::Equals([string]$key, $Name, [StringComparison]::OrdinalIgnoreCase)) {
            return [string]$Response.Headers[$key]
        }
    }
    return ""
}

function Wait-HttpReady {
    param(
        [string]$Uri,
        [string]$HostHeader,
        [int]$Seconds,
        [System.Diagnostics.Process]$Process,
        [string]$ErrorLog,
        [string]$WorkDirectory,
        [string]$ResolveAddress
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($null -ne $Process -and $Process.HasExited) {
            throw "process exited with code $($Process.ExitCode): $(Get-LogTail $ErrorLog)"
        }
        try {
            $response = Invoke-CurlResponse $Uri GET $HostHeader $WorkDirectory $ResolveAddress
            if ($response.StatusCode -eq 200) {
                return
            }
        } catch {
        }
        Start-Sleep -Milliseconds 250
    }
    throw "service did not become ready at ${Uri}: $(Get-LogTail $ErrorLog)"
}

# 用路径、大小和哈希建立发布根快照，验证读取链路没有改写资源。
function Get-TreeSnapshot {
    param([string]$Root)
    return @(
        Get-ChildItem -LiteralPath $Root -Recurse -Force |
            Sort-Object FullName |
            ForEach-Object {
                if ($_.PSIsContainer) {
                    "D|$($_.FullName)"
                } else {
                    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
                    "F|{0}|{1}|{2}" -f $_.FullName, $_.Length, $hash
                }
            }
    )
}

function Send-WebSocketText {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [string]$Text)
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    $segment = [ArraySegment[byte]]::new($bytes)
    $Socket.SendAsync(
        $segment,
        [System.Net.WebSockets.WebSocketMessageType]::Text,
        $true,
        [Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()
}

function Receive-WebSocketText {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [int]$TimeoutMilliseconds)
    $cts = [Threading.CancellationTokenSource]::new($TimeoutMilliseconds)
    $stream = [IO.MemoryStream]::new()
    try {
        do {
            $buffer = [byte[]]::new(65536)
            $segment = [ArraySegment[byte]]::new($buffer)
            $result = $Socket.ReceiveAsync($segment, $cts.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                return $null
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        return [Text.Encoding]::UTF8.GetString($stream.ToArray())
    } catch {
        if ($_.Exception -is [OperationCanceledException] -or
            $_.Exception.InnerException -is [OperationCanceledException]) {
            return $null
        }
        throw
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
    $parts = [Collections.Generic.List[string]]::new()
    foreach ($segment in @($message)) {
        if ($segment.type -eq "text") {
            [void]$parts.Add([string]$segment.data.text)
        } elseif ($segment.type -eq "markdown") {
            [void]$parts.Add([string]$segment.data.content)
        } elseif ($segment.type -eq "image") {
            [void]$parts.Add("[image:$([string]$segment.data.file)]")
        }
    }
    return ($parts -join "")
}

# 以反向 WebSocket 模拟最小 OneBot 会话，并确认插件实际发出的媒体 URL。
function Invoke-OneBotCommand {
    param(
        [string]$Endpoint,
        [string]$Message,
        [string]$UserId = "20001",
        [string]$SelfId = "10001"
    )
    $socket = [System.Net.WebSockets.ClientWebSocket]::new()
    try {
        $socket.ConnectAsync([Uri]$Endpoint, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
        $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        Send-WebSocketText $socket (([ordered]@{
            time = $now
            self_id = [int64]$SelfId
            post_type = "meta_event"
            meta_event_type = "lifecycle"
            sub_type = "connect"
        } | ConvertTo-Json -Compress -Depth 12))
        $messageId = [int64](Get-Random -Minimum 100000 -Maximum 999999999)
        $event = [ordered]@{
            time = $now
            self_id = [int64]$SelfId
            post_type = "message"
            message_type = "private"
            sub_type = "friend"
            message_id = $messageId
            message_seq = $messageId
            user_id = [int64]$UserId
            message = @([ordered]@{ type = "text"; data = [ordered]@{ text = $Message } })
            message_format = "array"
            raw_message = $Message
            font = 14
            sender = [ordered]@{ user_id = [int64]$UserId; nickname = "douluo-media-smoke" }
        }
        Send-WebSocketText $socket ($event | ConvertTo-Json -Compress -Depth 20)
        $payload = Receive-WebSocketText $socket 10000
        Assert-Condition (-not [string]::IsNullOrWhiteSpace($payload)) "command '$Message' produced no action"
        $action = $payload | ConvertFrom-Json
        Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$action.action)) "command '$Message' produced an invalid action"
        $response = [ordered]@{
            status = "ok"
            retcode = 0
            data = [ordered]@{ message_id = [int64](Get-Random -Minimum 900000 -Maximum 999999999) }
            message = ""
            wording = ""
            echo = $action.echo
        }
        Send-WebSocketText $socket ($response | ConvertTo-Json -Compress -Depth 20)
        [pscustomobject]@{
            Action = $action
            Text = Get-ActionText $action
            Json = $payload
        }
    } finally {
        if ($socket.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
            $socket.Abort()
        }
        $socket.Dispose()
    }
}

if ([string]::IsNullOrWhiteSpace($HostWorktree)) {
    throw "provide -HostWorktree or set QIMENBOT_SOURCE_DIR to a QimenBot source checkout"
}
$HostWorktree = Resolve-RequiredPath $HostWorktree "QimenBot source checkout"
if ([string]::IsNullOrWhiteSpace($PluginDll)) {
    $PluginDll = Join-Path $PSScriptRoot "..\target\release\qimen_dynamic_plugin_douluo_game.dll"
}
if ([string]::IsNullOrWhiteSpace($HostBinary)) {
    $HostBinary = Join-Path $HostWorktree "target\debug\qimenbotd.exe"
}
if ([string]::IsNullOrWhiteSpace($MediaBinary)) {
    $MediaBinary = Join-Path $PSScriptRoot "..\services\douluo-media\target\release\douluo-media.exe"
}
if ([string]::IsNullOrWhiteSpace($CaddyBinary)) {
    $caddyCommand = @(Get-Command caddy.exe -ErrorAction SilentlyContinue) | Select-Object -First 1
    if ($null -ne $caddyCommand) {
        $CaddyBinary = $caddyCommand.Source
    }
}
$PluginDll = Resolve-RequiredPath $PluginDll "plugin DLL"
$HostBinary = Resolve-RequiredPath $HostBinary "qimenbotd"
$MediaBinary = Resolve-RequiredPath $MediaBinary "douluo-media binary"
$CaddyBinary = Resolve-RequiredPath $CaddyBinary "Caddy binary"
$caddyConfigTemplate = Resolve-RequiredPath (Join-Path $PSScriptRoot "..\deploy\douluo-media\Caddyfile") "Caddyfile"

if ($AdminPort -eq 0) { $AdminPort = Get-FreePort }
if ($OneBotPort -eq 0) { $OneBotPort = Get-FreePort }
if ($MediaPort -eq 0) { $MediaPort = Get-FreePort }
if ($ProxyPort -eq 0) { $ProxyPort = Get-FreePort }
$ports = @($AdminPort, $OneBotPort, $MediaPort, $ProxyPort)
Assert-Condition ($ports.Count -eq (@($ports | Sort-Object -Unique).Count)) "smoke ports must be distinct"

$root = Join-Path ([IO.Path]::GetTempPath()) ("douluo-media-smoke-" + [Guid]::NewGuid().ToString("N"))
$published = Join-Path $root "published"
$assetPath = Join-Path $published ($AssetKey.Replace('/', '\'))
$assetDirectory = Split-Path -Parent $assetPath
$configDir = Join-Path $root "config"
$pluginBin = Join-Path $root "plugin-bin"
$pluginConfigDir = Join-Path $configDir "plugins"
$dataDir = Join-Path $root "data"
$logDir = Join-Path $root "logs"
$caddyData = Join-Path $root "caddy-data"
$null = New-Item -ItemType Directory -Force -Path $assetDirectory, $pluginBin, $pluginConfigDir, $dataDir, $logDir, $caddyData
# 使用可解码的 1x1 WebP 固定夹具，避免仅凭 RIFF 文件头通过链路验收。
$fixtureWebp = [Convert]::FromBase64String(
    "UklGRkAAAABXRUJQVlA4WAoAAAAQAAAAAAAAAAAAQUxQSAIAAAAAAFZQOCAYAAAAMAEAnQEqAQABAAIANCWkAANwAP77/VAA"
)
[IO.File]::WriteAllBytes($assetPath, $fixtureWebp)
$beforePublished = Get-TreeSnapshot $published

$mediaConfig = Join-Path $root "Caddyfile"
Copy-Item -LiteralPath $caddyConfigTemplate -Destination $mediaConfig
$mediaHost = "$TestHost`:$ProxyPort"
$publicBaseUrl = "https://$mediaHost$BasePath"
$mediaLog = Join-Path $logDir "douluo-media.stdout.log"
$mediaErrorLog = Join-Path $logDir "douluo-media.stderr.log"
$caddyLog = Join-Path $logDir "caddy.stdout.log"
$caddyErrorLog = Join-Path $logDir "caddy.stderr.log"
$hostLog = Join-Path $logDir "qimenbotd.stdout.log"
$hostErrorLog = Join-Path $logDir "qimenbotd.stderr.log"
$mediaProcess = $null
$caddyProcess = $null
$hostProcess = $null

try {
    $mediaProcess = Start-WithEnvironment `
        -FilePath $MediaBinary `
        -Arguments @() `
        -WorkingDirectory $root `
        -Environment @{
            DOULUO_MEDIA_BIND = "127.0.0.1:$MediaPort"
            DOULUO_MEDIA_ROOT = $published
        } `
        -StdoutPath $mediaLog `
        -StderrPath $mediaErrorLog
    Wait-HttpReady "http://127.0.0.1:$MediaPort/readyz" "" $TimeoutSeconds $mediaProcess $mediaErrorLog $root
    $internal = Invoke-CurlResponse "http://127.0.0.1:$MediaPort/media/$AssetKey" GET "" $root
    Assert-Condition ($internal.StatusCode -eq 200) "media service returned HTTP $($internal.StatusCode)"
    Assert-Condition ((Get-HeaderValue $internal "Content-Type") -eq "image/webp") "media service returned an unexpected MIME"
    Assert-Condition ([Convert]::ToBase64String($internal.BodyBytes) -eq [Convert]::ToBase64String([IO.File]::ReadAllBytes($assetPath))) "media bytes changed before proxy"

    $caddyStorage = $caddyData.Replace('\', '/')
    $caddyEnv = @{
        MEDIA_HOST = "https://$mediaHost"
        MEDIA_TLS = "tls internal"
        MEDIA_BASE_PATH = $BasePath
        MEDIA_UPSTREAM = "127.0.0.1:$MediaPort"
        CADDY_STORAGE_ROOT = $caddyStorage
    }
    $adaptLog = Join-Path $logDir "caddy-adapt.log"
    $savedCaddyEnv = @{}
    try {
        foreach ($key in $caddyEnv.Keys) {
            $savedCaddyEnv[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
            [Environment]::SetEnvironmentVariable($key, [string]$caddyEnv[$key], "Process")
        }
        $adaptProcess = Start-Process `
            -FilePath $CaddyBinary `
            -ArgumentList (Join-ProcessArguments @("adapt", "--config", $mediaConfig, "--adapter", "caddyfile", "--pretty")) `
            -WorkingDirectory $root `
            -RedirectStandardOutput $adaptLog `
            -RedirectStandardError (Join-Path $logDir "caddy-adapt.stderr.log") `
            -WindowStyle Hidden `
            -PassThru `
            -Wait
    } finally {
        foreach ($key in $savedCaddyEnv.Keys) {
            [Environment]::SetEnvironmentVariable($key, $savedCaddyEnv[$key], "Process")
        }
    }
    Assert-Condition ($adaptProcess.ExitCode -eq 0) "Caddyfile adaptation failed: $(Get-LogTail $adaptLog)"
    $caddyProcess = Start-WithEnvironment `
        -FilePath $CaddyBinary `
        -Arguments @("run", "--config", $mediaConfig, "--adapter", "caddyfile") `
        -WorkingDirectory $root `
        -Environment $caddyEnv `
        -StdoutPath $caddyLog `
        -StderrPath $caddyErrorLog
    Wait-HttpReady "$publicBaseUrl/readyz" $mediaHost $TimeoutSeconds $caddyProcess $caddyErrorLog $root "127.0.0.1"

    $proxyReady = Invoke-CurlResponse "$publicBaseUrl/readyz" GET $mediaHost $root "127.0.0.1"
    Assert-Condition ($proxyReady.StatusCode -eq 200) "Caddy base-path ready check returned HTTP $($proxyReady.StatusCode)"
    Assert-Condition ((Get-HeaderValue $proxyReady "X-Content-Type-Options") -eq "nosniff") "Caddy did not add nosniff"
    Assert-Condition ((Get-HeaderValue $proxyReady "X-Frame-Options") -eq "DENY") "Caddy did not add frame policy"
    $proxyAsset = Invoke-CurlResponse "$publicBaseUrl/media/$AssetKey" GET $mediaHost $root "127.0.0.1"
    Assert-Condition ($proxyAsset.StatusCode -eq 200) "Caddy media proxy returned HTTP $($proxyAsset.StatusCode)"
    Assert-Condition ([Convert]::ToBase64String($proxyAsset.BodyBytes) -eq [Convert]::ToBase64String([IO.File]::ReadAllBytes($assetPath))) "Caddy changed media bytes"
    $outsideBase = Invoke-CurlResponse "https://$mediaHost/media/$AssetKey" GET $mediaHost $root "127.0.0.1"
    Assert-Condition ($outsideBase.StatusCode -eq 404) "media route was reachable without the configured base path"
    $listing = Invoke-CurlResponse "$publicBaseUrl/" GET $mediaHost $root "127.0.0.1"
    Assert-Condition ($listing.StatusCode -eq 404) "media service exposed a directory listing"
    $post = Invoke-CurlResponse "$publicBaseUrl/media/$AssetKey" POST $mediaHost $root "127.0.0.1"
    Assert-Condition ($post.StatusCode -eq 405) "write method was not rejected by the read-only service"
    Write-Output "media service and Caddy HTTPS/base-path checks passed"

    Copy-Item -LiteralPath $PluginDll -Destination (Join-Path $pluginBin "qimen_dynamic_plugin_douluo_game.dll")
    $tomlBin = $pluginBin.Replace('\', '/')
    $tomlConfigDir = $pluginConfigDir.Replace('\', '/')
    $tomlState = (Join-Path $configDir "plugin-state.toml").Replace('\', '/')
    $tomlAudit = (Join-Path $configDir "admin-audit.jsonl").Replace('\', '/')
    $hostConfig = @"
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
access_token = "media-remote-smoke"
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
id = "douluo-media-smoke"
account_id = "10001"
protocol = "onebot11"
transport = "ws-reverse"
bind = "127.0.0.1:$OneBotPort"
path = "/onebot/media-remote-smoke"
enabled = true
enabled_modules = ["command"]
owners = ["20001"]
admins = []
auto_reply_poke_enabled = false
"@
    Set-Content -LiteralPath (Join-Path $configDir "base.toml") -Value $hostConfig -Encoding UTF8
    Set-Content -LiteralPath (Join-Path $configDir "plugin-state.toml") -Value "[modules]`n`"douluo-game`" = true`n" -Encoding UTF8
    $pluginConfig = @"
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[identity]
namespace = "media-remote-smoke"

[authorization]
mode = "allow_all"

[illustrations]
enabled = true
mode = "remote"
remote_base_url = "$publicBaseUrl"

[messages]
qq_official_markdown = true
onebot_markdown = true
legacy_hyphen_arguments = true
"@
    Set-Content -LiteralPath (Join-Path $pluginConfigDir "douluo-game.toml") -Value $pluginConfig -Encoding UTF8

    $oldConfigPath = $env:QIMEN_CONFIG_PATH
    $env:QIMEN_CONFIG_PATH = "config/base.toml"
    try {
        $hostProcess = Start-Process `
            -FilePath $HostBinary `
            -WorkingDirectory $root `
            -RedirectStandardOutput $hostLog `
            -RedirectStandardError $hostErrorLog `
            -WindowStyle Hidden `
            -PassThru
    } finally {
        if ($null -eq $oldConfigPath) {
            Remove-Item Env:QIMEN_CONFIG_PATH -ErrorAction SilentlyContinue
        } else {
            $env:QIMEN_CONFIG_PATH = $oldConfigPath
        }
    }
    Wait-HttpReady "http://127.0.0.1:$AdminPort/healthz" "" $TimeoutSeconds $hostProcess $hostErrorLog $root
    $endpoint = "ws://127.0.0.1:$OneBotPort/onebot/media-remote-smoke"
    $start = Invoke-OneBotCommand $endpoint "开始穿越 远程图片 男"
    Assert-Condition ($start.Text.Contains("穿越成功")) "isolated host did not initialize a player"
    $position = Invoke-OneBotCommand $endpoint "位置"
    $expectedUrl = "$publicBaseUrl/media/$AssetKey"
    Assert-Condition ($position.Text.Contains($expectedUrl)) "plugin remote mode did not emit the expected URL: $($position.Json)"
    $remoteAsset = Invoke-CurlResponse "$publicBaseUrl/media/$AssetKey" GET $mediaHost $root "127.0.0.1"
    Assert-Condition ($remoteAsset.StatusCode -eq 200) "URL emitted by plugin was not readable through the HTTPS proxy"
    Assert-Condition ([Convert]::ToBase64String($remoteAsset.BodyBytes) -eq [Convert]::ToBase64String([IO.File]::ReadAllBytes($assetPath))) "plugin URL did not resolve to published bytes"
    Write-Output "plugin remote mode and isolated OneBot URL-to-bytes integration passed"

    $afterPublished = Get-TreeSnapshot $published
    Assert-Condition ((Compare-Object $beforePublished $afterPublished).Count -eq 0) "published root changed during read-only smoke"
    Write-Output "published root remained unchanged during service, proxy, and plugin checks"
} catch {
    throw
} finally {
    Stop-ProcessIfRunning $hostProcess
    Stop-ProcessIfRunning $caddyProcess
    Stop-ProcessIfRunning $mediaProcess
    if ($KeepTemp) {
        Write-Output "kept temporary smoke root: $root"
    } else {
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    }
}
