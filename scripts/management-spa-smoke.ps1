[CmdletBinding()]
param(
    [string]$HostWorktree = $env:QIMENBOT_SOURCE_DIR,
    [string]$PluginDll = "",
    [string]$HostBinary = "",
    [int]$ManagementPort = 0,
    [int]$BotPort = 0,
    [int]$TimeoutSeconds = 30,
    [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"
$AdminSecret = "smoke-" + [Guid]::NewGuid().ToString("N")

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "management SPA smoke assertion failed: $Message"
    }
}

function Convert-ToTomlPath {
    param([string]$Path)
    return $Path.Replace("\", "/").Replace('"', '\"')
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

function Get-HeaderValue {
    param($Response, [string]$Name)
    $value = $Response.Headers[$Name]
    if ($value -is [array]) {
        return ($value -join ",")
    }
    return [string]$value
}

function Read-ErrorResponseBody {
    param($Response)
    $stream = $Response.GetResponseStream()
    if ($null -eq $stream) {
        return ""
    }
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8)
    try {
        return $reader.ReadToEnd()
    } finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Invoke-HttpResponse {
    param(
        [string]$Uri,
        [ValidateSet("Get", "Post", "Delete")]
        [string]$Method = "Get",
        [hashtable]$Headers = @{},
        [string]$Body = $null,
        [object]$WebSession = $null
    )
    $parameters = @{
        Uri = $Uri
        Method = $Method
        UseBasicParsing = $true
        TimeoutSec = 5
    }
    if ($Headers.Count -gt 0) {
        $parameters.Headers = $Headers
    }
    if (-not [string]::IsNullOrEmpty($Body)) {
        $parameters.Body = $Body
        $parameters.ContentType = "application/json"
    }
    if ($null -ne $WebSession) {
        $parameters.WebSession = $WebSession
    }

    try {
        $response = Invoke-WebRequest @parameters
        return [pscustomobject]@{
            StatusCode = [int]$response.StatusCode
            Headers = $response.Headers
            Content = [string]$response.Content
            Raw = $response
        }
    } catch {
        $response = $_.Exception.Response
        if ($null -eq $response) {
            throw
        }
        return [pscustomobject]@{
            StatusCode = [int]$response.StatusCode
            Headers = $response.Headers
            Content = Read-ErrorResponseBody $response
            Raw = $null
        }
    }
}

function Assert-SecurityHeaders {
    param($Response, [string]$ExpectedCsp)
    Assert-Condition ((Get-HeaderValue $Response "Cache-Control") -eq "no-store") "missing no-store cache policy"
    Assert-Condition ((Get-HeaderValue $Response "X-Content-Type-Options") -eq "nosniff") "missing nosniff header"
    Assert-Condition ((Get-HeaderValue $Response "X-Frame-Options") -eq "DENY") "missing DENY frame policy"
    Assert-Condition ((Get-HeaderValue $Response "Referrer-Policy") -eq "no-referrer") "missing referrer policy"
    if (-not [string]::IsNullOrEmpty($ExpectedCsp)) {
        $actualCsp = Get-HeaderValue $Response "Content-Security-Policy"
        Assert-Condition ($actualCsp -eq $ExpectedCsp) "unexpected static asset CSP: actual='$actualCsp' expected='$ExpectedCsp'"
    }
}

function Convert-ResponseJson {
    param($Response, [string]$Description)
    Assert-Condition ($Response.StatusCode -ge 200 -and $Response.StatusCode -lt 300) "$Description returned HTTP $($Response.StatusCode): $($Response.Content)"
    try {
        return $Response.Content | ConvertFrom-Json
    } catch {
        throw "$Description returned invalid JSON: $($Response.Content)"
    }
}

function Wait-Health {
    param([string]$BaseUrl, [int]$Seconds, [System.Diagnostics.Process]$Process, [string]$ErrorLog)
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($null -ne $Process -and $Process.HasExited) {
            $tail = if (Test-Path -LiteralPath $ErrorLog) { Get-Content -Raw -LiteralPath $ErrorLog } else { "" }
            throw "isolated qimenbotd exited with code $($Process.ExitCode): $tail"
        }
        try {
            $health = Invoke-HttpResponse -Uri "$BaseUrl/healthz"
            if ($health.StatusCode -eq 200) {
                $ready = Invoke-HttpResponse -Uri "$BaseUrl/readyz"
                if ($ready.StatusCode -eq 200) {
                    return
                }
            }
        } catch {
        }
        Start-Sleep -Milliseconds 250
    }
    $tail = if (Test-Path -LiteralPath $ErrorLog) { Get-Content -Raw -LiteralPath $ErrorLog } else { "" }
    throw "management server did not become ready at ${BaseUrl}: $tail"
}

function Assert-StaticAsset {
    param(
        [string]$BaseUrl,
        [string]$Path,
        [string]$ExpectedContentType,
        [string]$ExpectedCsp
    )
    $response = Invoke-HttpResponse -Uri "$BaseUrl$Path"
    Assert-Condition ($response.StatusCode -eq 200) "static asset $Path returned HTTP $($response.StatusCode)"
    Assert-Condition ((Get-HeaderValue $response "Content-Type") -eq $ExpectedContentType) "static asset $Path has Content-Type '$((Get-HeaderValue $response "Content-Type"))'"
    Assert-SecurityHeaders $response $ExpectedCsp
    return $response
}

function Assert-CursorPage {
    param([string]$BaseUrl, [string]$Path, [object]$WebSession)
    $response = Invoke-HttpResponse -Uri "$BaseUrl$Path" -WebSession $WebSession
    $payload = Convert-ResponseJson $response $Path
    $propertyNames = @($payload.PSObject.Properties.Name)
    Assert-Condition ($propertyNames -contains "entries") "$Path response has no entries field"
    Assert-Condition ($propertyNames -contains "next_after_id") "$Path response has no next_after_id field"
    $entryCount = if ($null -eq $payload.entries) { 0 } else { @($payload.entries).Count }
    Assert-Condition ($entryCount -le 1) "$Path ignored limit=1"
    if ($null -ne $payload.next_after_id) {
        $afterId = [long]$payload.next_after_id
        $separator = if ($Path.Contains("?")) { "&" } else { "?" }
        $nextPath = "$Path$separator`after_id=$afterId&limit=1"
        $nextResponse = Invoke-HttpResponse -Uri "$BaseUrl$nextPath" -WebSession $WebSession
        $nextPayload = Convert-ResponseJson $nextResponse $nextPath
        Assert-Condition (@($nextPayload.PSObject.Properties.Name) -contains "entries") "$nextPath response has no entries field"
    }
    return [int]$entryCount
}

if ([string]::IsNullOrWhiteSpace($HostWorktree)) {
    throw "provide -HostWorktree or set QIMENBOT_SOURCE_DIR to a QimenBot source checkout"
}
$HostWorktree = (Resolve-Path -LiteralPath $HostWorktree).Path
if ([string]::IsNullOrWhiteSpace($PluginDll)) {
    $PluginDll = Join-Path $PSScriptRoot "..\target\release\qimen_dynamic_plugin_douluo_game.dll"
}
if ([string]::IsNullOrWhiteSpace($HostBinary)) {
    $HostBinary = Join-Path $HostWorktree "target\debug\qimenbotd.exe"
}
$PluginDll = (Resolve-Path -LiteralPath $PluginDll).Path
$HostBinary = (Resolve-Path -LiteralPath $HostBinary).Path
Assert-Condition (Test-Path -LiteralPath $PluginDll) "plugin DLL not found: $PluginDll"
Assert-Condition (Test-Path -LiteralPath $HostBinary) "isolated qimenbotd not found: $HostBinary"

if ($ManagementPort -eq 0) {
    $ManagementPort = Get-FreePort
}
if ($BotPort -eq 0) {
    $BotPort = Get-FreePort
}
Assert-Condition ($ManagementPort -ne $BotPort) "management and bot ports must be different"

$root = Join-Path ([IO.Path]::GetTempPath()) ("douluo-management-smoke-" + [Guid]::NewGuid().ToString("N"))
$bin = Join-Path $root "plugin-bin"
$configDir = Join-Path $root "config"
$pluginConfigDir = Join-Path $configDir "plugins"
$logDir = Join-Path $root "logs"
$null = New-Item -ItemType Directory -Force -Path $bin, $pluginConfigDir, $logDir
Copy-Item -LiteralPath $PluginDll -Destination (Join-Path $bin "qimen_dynamic_plugin_douluo_game.dll")

$tomlBin = Convert-ToTomlPath $bin
$tomlConfigDir = Convert-ToTomlPath $pluginConfigDir
$tomlState = Convert-ToTomlPath (Join-Path $configDir "plugin-state.toml")
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
enabled = false
bind = "127.0.0.1:3210"
access_token = ""
log_capacity = 2000
audit_path = "config/admin-audit.jsonl"

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
id = "douluo-management-smoke"
account_id = "10001"
protocol = "onebot11"
transport = "ws-reverse"
bind = "127.0.0.1:$BotPort"
path = "/onebot/management-smoke"
enabled = true
enabled_modules = ["command"]
owners = ["20001"]
admins = []
auto_reply_poke_enabled = false
"@

$pluginState = @'
[modules]
"douluo-game" = true
'@

$pluginConfig = @"
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[identity]
namespace = "management-smoke"

[authorization]
mode = "allow_all"

[illustrations]
enabled = false
mode = "direct"

[web]
enabled = true
bind = "127.0.0.1"
port = $ManagementPort
admin_secret = "$AdminSecret"

[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
"@

Set-Content -LiteralPath (Join-Path $configDir "base.toml") -Value $hostConfig -Encoding UTF8
Set-Content -LiteralPath (Join-Path $configDir "plugin-state.toml") -Value $pluginState -Encoding UTF8
Set-Content -LiteralPath (Join-Path $pluginConfigDir "douluo-game.toml") -Value $pluginConfig -Encoding UTF8

$stdoutLog = Join-Path $logDir "qimenbotd.stdout.log"
$stderrLog = Join-Path $logDir "qimenbotd.stderr.log"
$baseUrl = "http://127.0.0.1:$ManagementPort"
$oldConfigPath = $env:QIMEN_CONFIG_PATH
$env:QIMEN_CONFIG_PATH = "config/base.toml"
$process = $null
try {
    $process = Start-Process -FilePath $HostBinary -WorkingDirectory $root -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -WindowStyle Hidden -PassThru
} finally {
    if ($null -eq $oldConfigPath) {
        Remove-Item Env:QIMEN_CONFIG_PATH -ErrorAction SilentlyContinue
    } else {
        $env:QIMEN_CONFIG_PATH = $oldConfigPath
    }
}

try {
    Wait-Health $baseUrl $TimeoutSeconds $process $stderrLog
    $staticCsp = "default-src 'none'; base-uri 'none'; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self'"
    $index = Assert-StaticAsset $baseUrl "/" "text/html; charset=utf-8" $staticCsp
    $assetPaths = @(
        [regex]::Matches($index.Content, '/assets/[^"''\s]+') |
            ForEach-Object { $_.Value } |
            Sort-Object -Unique
    )
    $jsPath = $assetPaths | Where-Object { $_ -match '\.js$' } | Select-Object -First 1
    $cssPath = $assetPaths | Where-Object { $_ -match '\.css$' } | Select-Object -First 1
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($jsPath)) "index.html has no JavaScript asset"
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($cssPath)) "index.html has no CSS asset"
    $null = Assert-StaticAsset $baseUrl $jsPath "text/javascript; charset=utf-8" $staticCsp
    $css = Assert-StaticAsset $baseUrl $cssPath "text/css; charset=utf-8" $staticCsp
    $fontPath = [regex]::Matches($css.Content, '/assets/[^)"''\s]+\.woff2') |
        ForEach-Object { $_.Value } |
        Select-Object -First 1
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($fontPath)) "CSS has no WOFF2 asset"
    $null = Assert-StaticAsset $baseUrl $fontPath "font/woff2" $staticCsp
    $missingAsset = Invoke-HttpResponse -Uri "$baseUrl/assets/management-smoke-missing.js"
    Assert-Condition ($missingAsset.StatusCode -eq 404) "missing static asset returned HTTP $($missingAsset.StatusCode)"
    Assert-SecurityHeaders $missingAsset $null
    Write-Output "static page and JS/CSS/WOFF2 MIME/CSP checks passed"

    $unauthenticated = Invoke-HttpResponse -Uri "$baseUrl/api/v1/content/active"
    Assert-Condition ($unauthenticated.StatusCode -eq 401) "unauthenticated active request returned HTTP $($unauthenticated.StatusCode)"
    $badLoginBody = (@{ secret = "wrong-$AdminSecret" } | ConvertTo-Json -Compress)
    $badLogin = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -Method Post -Body $badLoginBody
    Assert-Condition ($badLogin.StatusCode -eq 401) "invalid login returned HTTP $($badLogin.StatusCode)"

    $session = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $loginBody = (@{ secret = $AdminSecret } | ConvertTo-Json -Compress)
    $login = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -Method Post -Body $loginBody -WebSession $session
    $loginPayload = Convert-ResponseJson $login "session login"
    Assert-Condition ([string]$loginPayload.role -eq "content_admin") "login returned an unexpected role"
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$loginPayload.csrf_token)) "login returned no CSRF token"
    $csrfToken = [string]$loginPayload.csrf_token
    $setCookie = @($login.Headers["Set-Cookie"]) | Select-Object -First 1
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$setCookie)) "login returned no session cookie"
    Assert-Condition ([string]$setCookie -match "HttpOnly") "session cookie is not HttpOnly"
    Assert-Condition ([string]$setCookie -match "SameSite=Strict") "session cookie is not SameSite=Strict"
    $cookieHeader = ([string]$setCookie -split ";")[0]
    Assert-Condition ($cookieHeader -match '^douluo_admin_session=[0-9a-f]{64}$') "session cookie has an unexpected format"

    $currentSession = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -WebSession $session
    $currentPayload = Convert-ResponseJson $currentSession "current session"
    Assert-Condition ([string]$currentPayload.role -eq "content_admin") "current session is not content_admin"

    $active = Invoke-HttpResponse -Uri "$baseUrl/api/v1/content/active" -WebSession $session
    $activePayload = Convert-ResponseJson $active "active content"
    Assert-Condition (-not [string]::IsNullOrWhiteSpace([string]$activePayload.revision.package_key)) "active revision has no package key"

    foreach ($path in @(
        "/api/v1/content/drafts?limit=1",
        "/api/v1/content/revisions?limit=1",
        "/api/v1/content/activations?limit=1",
        "/api/v1/content/operations?limit=1",
        "/api/v1/content/rollback-operations?limit=1",
        "/api/v1/content/stage-operations?limit=1"
    )) {
        $count = Assert-CursorPage $baseUrl $path $session
        Write-Output ("read-only page {0}: {1} entry/entries" -f $path, $count)
    }
    Write-Output "session login and active/draft/revision/activation/audit read-only pagination checks passed"

    $logoutWithoutCsrf = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -Method Delete -WebSession $session
    Assert-Condition ($logoutWithoutCsrf.StatusCode -eq 403) "logout without CSRF returned HTTP $($logoutWithoutCsrf.StatusCode)"
    $logoutHeaders = @{ "X-CSRF-Token" = $csrfToken }
    $logout = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -Method Delete -Headers $logoutHeaders -WebSession $session
    Assert-Condition ($logout.StatusCode -in @(200, 204)) "logout returned HTTP $($logout.StatusCode)"
    Assert-Condition ((Get-HeaderValue $logout "Set-Cookie") -match "Max-Age=0") "logout did not expire the session cookie"
    $oldSession = New-Object Microsoft.PowerShell.Commands.WebRequestSession
    $oldCookieHeaders = @{ Cookie = $cookieHeader }
    $afterLogout = Invoke-HttpResponse -Uri "$baseUrl/api/v1/session" -Headers $oldCookieHeaders -WebSession $oldSession
    Assert-Condition ($afterLogout.StatusCode -eq 401) "expired session remained usable after logout"
    Write-Output "CSRF-protected logout check passed"
    Write-Output "management SPA smoke passed: isolated host, same-origin assets, security headers, session lifecycle, and read-only content pages"
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
