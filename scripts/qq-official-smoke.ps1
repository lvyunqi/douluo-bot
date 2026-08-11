[CmdletBinding()]
param(
    [string]$HostWorktree = $env:QIMENBOT_SOURCE_DIR,
    [string]$PluginDll = "",
    [string]$HostBinary = "",
    [int]$AdminPort = 3223,
    [int]$StartupTimeoutSeconds = 30,
    [switch]$StartGateway,
    [switch]$Sandbox,
    [switch]$KeepTemp
)

$ErrorActionPreference = "Stop"

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "QQ 官方冒烟失败: $Message"
    }
}

function Resolve-RequiredPath {
    param([string]$Path, [string]$Label, [switch]$Directory)
    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path)) {
        throw "$Label 不存在: $Path"
    }
    $item = Get-Item -LiteralPath $Path
    if ($Directory -and -not $item.PSIsContainer) {
        throw "$Label 必须是目录: $Path"
    }
    if (-not $Directory -and $item.PSIsContainer) {
        throw "$Label 必须是文件: $Path"
    }
    return $item.FullName
}

function Convert-ToTomlPath {
    param([string]$Path)
    return $Path.Replace("\", "/").Replace('"', '\"')
}

function Wait-AdminHealth {
    param(
        [string]$Url,
        [int]$TimeoutSeconds,
        [System.Diagnostics.Process]$Process,
        [string]$Root
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Process.HasExited) {
            throw "临时 qimenbotd 在健康检查前退出；日志保留在 $Root"
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
    throw "临时 qimenbotd 未在 $TimeoutSeconds 秒内就绪；日志保留在 $Root"
}

function Invoke-AdminJson {
    param([string]$Uri, [hashtable]$Headers)
    $response = Invoke-WebRequest -Uri $Uri -Headers $Headers -UseBasicParsing
    $stream = $response.RawContentStream
    $stream.Position = 0
    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8)
    try {
        return ($reader.ReadToEnd() | ConvertFrom-Json)
    } finally {
        $reader.Dispose()
    }
}

function Remove-SmokeRoot {
    param([string]$Root)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $fullRoot = [IO.Path]::GetFullPath($Root)
    $underTemp = $fullRoot.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)
    $safeName = ([IO.Path]::GetFileName($fullRoot) -like "douluo-qq-official-smoke-*")
    if (-not $underTemp -or -not $safeName) {
        throw "拒绝删除非预期临时目录: $fullRoot"
    }
    Remove-Item -LiteralPath $fullRoot -Recurse -Force
}

if ([string]::IsNullOrWhiteSpace($HostWorktree)) {
    throw "请提供 -HostWorktree 或设置 QIMENBOT_SOURCE_DIR"
}
$HostWorktree = Resolve-RequiredPath $HostWorktree "QimenBot 源码目录" -Directory
if (-not $PluginDll) {
    $PluginDll = Join-Path $PSScriptRoot "..\target\release\qimen_dynamic_plugin_douluo_game.dll"
}
if (-not $HostBinary) {
    $HostBinary = Join-Path $HostWorktree "target\debug\qimenbotd.exe"
}
$PluginDll = Resolve-RequiredPath $PluginDll "插件 DLL"
$HostBinary = Resolve-RequiredPath $HostBinary "qimenbotd"

$hasAppId = -not [string]::IsNullOrWhiteSpace($env:QQBOT_APPID)
$hasSecret = -not [string]::IsNullOrWhiteSpace($env:QQBOT_SECRET)
if (-not $StartGateway) {
    $summary = "QQ 官方 Gateway 前置检查通过: DLL={0}; qimenbotd={1}; QQBOT_APPID={2}; QQBOT_SECRET={3}" -f $PluginDll, $HostBinary, $hasAppId, $hasSecret
    Write-Output $summary
    Write-Output "未启动 Gateway。确认凭据和 GROUP_AND_C2C_EVENT 权限后，以 -StartGateway 显式执行人工群/C2C 验收。"
    return
}

Assert-Condition $hasAppId "缺少 QQBOT_APPID；不会尝试启动官方 Gateway"
Assert-Condition $hasSecret "缺少 QQBOT_SECRET；不会尝试启动官方 Gateway"
Assert-Condition ($AdminPort -ge 1 -and $AdminPort -le 65535) "AdminPort 必须在 1 到 65535 之间"
Assert-Condition ($StartupTimeoutSeconds -ge 5 -and $StartupTimeoutSeconds -le 120) "StartupTimeoutSeconds 必须在 5 到 120 之间"
$occupied = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Where-Object { $_.LocalPort -eq $AdminPort }
Assert-Condition ($null -eq $occupied) "管理端口 $AdminPort 已被占用"

$root = Join-Path ([IO.Path]::GetTempPath()) ("douluo-qq-official-smoke-" + [Guid]::NewGuid().ToString("N"))
$bin = Join-Path $root "plugin-bin"
$configDir = Join-Path $root "config"
$pluginConfigDir = Join-Path $configDir "plugins"
$assetRoot = Join-Path $root "douluo-game\assets"
$logDir = Join-Path $root "logs"
$process = $null
$failed = $true

try {
    $null = New-Item -ItemType Directory -Force -Path $bin, $pluginConfigDir, $assetRoot, $logDir
    Copy-Item -LiteralPath $PluginDll -Destination (Join-Path $bin "qimen_dynamic_plugin_douluo_game.dll")

    # 使用临时无版权风险的 1×1 WebP，验证 direct 图片只在主回复之后作为独立媒体发送。
    $testImage = Join-Path $assetRoot "maps\holy-soul-village\cover.webp"
    $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $testImage)
    [IO.File]::WriteAllBytes(
        $testImage,
        [Convert]::FromBase64String("UklGRiIAAABXRUJQVlA4IBYAAABwAQCdASoBAAEAAUAmJaQAA3AA/vuUAAA=")
    )

    $adminToken = "qq-smoke-" + [Guid]::NewGuid().ToString("N")
    $tomlBin = Convert-ToTomlPath $bin
    $tomlPluginConfig = Convert-ToTomlPath $pluginConfigDir
    $tomlState = Convert-ToTomlPath (Join-Path $configDir "plugin-state.toml")
    $tomlAudit = Convert-ToTomlPath (Join-Path $configDir "admin-audit.jsonl")
    $sandboxValue = ([bool]$Sandbox).ToString().ToLowerInvariant()
$config = @"
[runtime]
env = "qq-official-smoke"
shutdown_timeout_secs = 15
task_grace_secs = 5

[observability]
level = "info"
json_logs = false
metrics_bind = "127.0.0.1:0"

[admin_web]
enabled = true
bind = "127.0.0.1:$AdminPort"
access_token = "$adminToken"
log_capacity = 200
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
plugin_config_dir = "$tomlPluginConfig"
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

[[bots]]
id = "douluo-qq-official-smoke"
account_id = "`${QQBOT_APPID}"
protocol = "qq-official"
transport = "gateway"
appid = "`${QQBOT_APPID}"
secret = "`${QQBOT_SECRET}"
sandbox = $sandboxValue
intents = ["GROUP_AND_C2C_EVENT"]
enabled = true
enabled_modules = ["command"]
owners = []
admins = []
"@
    Set-Content -LiteralPath (Join-Path $configDir "base.toml") -Value $config -Encoding utf8
$pluginState = @'
[modules]
"douluo-game" = true
'@
    Set-Content -LiteralPath (Join-Path $configDir "plugin-state.toml") -Value $pluginState -Encoding utf8
$pluginConfig = @'
[database]
relative_path = "douluo-game/douluo.db"
busy_timeout_ms = 3000

[identity]
namespace = "qq-official-smoke"

[authorization]
mode = "allow_all"

[illustrations]
enabled = true
mode = "direct"
direct_asset_root = "douluo-game/assets"

[messages]
qq_official_markdown = true
onebot_markdown = false
legacy_hyphen_arguments = true
'@
    Set-Content -LiteralPath (Join-Path $pluginConfigDir "douluo-game.toml") -Value $pluginConfig -Encoding utf8

    $stdoutLog = Join-Path $logDir "qimenbotd.stdout.log"
    $stderrLog = Join-Path $logDir "qimenbotd.stderr.log"
    $previousConfigPath = $env:QIMEN_CONFIG_PATH
    try {
        $env:QIMEN_CONFIG_PATH = "config/base.toml"
        $process = Start-Process -FilePath $HostBinary -WorkingDirectory $root -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog -WindowStyle Hidden -PassThru
    } finally {
        if ($null -eq $previousConfigPath) {
            Remove-Item Env:QIMEN_CONFIG_PATH -ErrorAction SilentlyContinue
        } else {
            $env:QIMEN_CONFIG_PATH = $previousConfigPath
        }
    }

    Wait-AdminHealth "http://127.0.0.1:$AdminPort/healthz" $StartupTimeoutSeconds $process $root
    $plugins = Invoke-AdminJson "http://127.0.0.1:$AdminPort/api/v1/plugins" @{ Authorization = "Bearer $adminToken" }
    $plugin = @($plugins.data) | Where-Object { $_.id -eq "douluo-game" } | Select-Object -First 1
    Assert-Condition ($null -ne $plugin) "动态插件描述符未加载"
    Assert-Condition ([bool]$plugin.loaded) "动态插件初始化失败"

    Write-Output "QQ 官方 Gateway 已就绪。请在已授权测试群 @ 机器人发送“斗罗系统”，再以 C2C 发送“斗罗系统”。"
    Write-Output "每个场景应先看到完整 Markdown/文字，再看到一张独立 1×1 WebP；频道/DMS 不在本次验收范围。"
    Write-Output "完成客户端核验后按 Enter 停止临时宿主。脚本不会打印凭据、Base64 或原始消息。"
    Read-Host | Out-Null
    $failed = $false
} finally {
    if ($null -ne $process -and -not $process.HasExited) {
        $process.Kill()
        $process.WaitForExit()
    }
    if ($KeepTemp -or $failed) {
        Write-Output "临时 QQ 冒烟目录保留在: $root"
    } elseif (Test-Path -LiteralPath $root) {
        Remove-SmokeRoot $root
    }
}
