[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$LegacyMediaRoot,

    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$QuarantineRoot,

    [ValidateRange(128, 2048)]
    [int]$SoulRingMaxEdge = 640,

    [ValidateRange(512, 2048)]
    [int]$WorldMapMaxEdge = 1280,

    [ValidateRange(1, 100)]
    [int]$JpegQuality = 85
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

function Resolve-ExistingDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$Label
    )

    $item = Get-Item -LiteralPath $Path
    if (-not $item.PSIsContainer) {
        throw "$Label 必须是目录：$Path"
    }
    return $item.FullName
}

function Initialize-EmptyQuarantineRoot {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$LegacyRoot
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $legacyPrefix = $LegacyRoot.TrimEnd([char[]]@([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)) + [IO.Path]::DirectorySeparatorChar
    if ($fullPath.Equals($LegacyRoot, [StringComparison]::OrdinalIgnoreCase) -or $fullPath.StartsWith($legacyPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw '隔离输出目录不能位于旧媒体源目录内'
    }

    if (Test-Path -LiteralPath $fullPath) {
        $item = Get-Item -LiteralPath $fullPath
        if (-not $item.PSIsContainer) {
            throw "隔离输出路径不是目录：$fullPath"
        }
        if ($null -ne (Get-ChildItem -LiteralPath $fullPath -Force | Select-Object -First 1)) {
            throw "隔离输出目录必须为空，避免混入或覆盖既有文件：$fullPath"
        }
    } else {
        [void](New-Item -ItemType Directory -Path $fullPath)
    }

    $notice = "这些文件来自旧媒体，只用于格式核验和聊天尺寸预览。`r`n它们没有获得发布许可，不能复制到 published root、公开仓库、CDN 或 QQ/OneBot 发送路径。`r`n"
    Set-Content -LiteralPath (Join-Path $fullPath 'QUARANTINE-DO-NOT-PUBLISH.txt') -Encoding utf8 -NoNewline -Value $notice
    return $fullPath
}

function Get-ImageMime {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    $header = [byte[]]::new(12)
    $stream = [IO.File]::OpenRead($Path)
    try {
        $length = 0
        while ($length -lt $header.Length) {
            $read = $stream.Read($header, $length, $header.Length - $length)
            if ($read -eq 0) {
                break
            }
            $length += $read
        }
    } finally {
        $stream.Dispose()
    }

    if ($length -ge 8 -and $header[0] -eq 0x89 -and $header[1] -eq 0x50 -and $header[2] -eq 0x4e -and $header[3] -eq 0x47 -and $header[4] -eq 0x0d -and $header[5] -eq 0x0a -and $header[6] -eq 0x1a -and $header[7] -eq 0x0a) {
        return 'image/png'
    }
    if ($length -ge 3 -and $header[0] -eq 0xff -and $header[1] -eq 0xd8 -and $header[2] -eq 0xff) {
        return 'image/jpeg'
    }
    if ($length -ge 12 -and [Text.Encoding]::ASCII.GetString($header, 0, 4) -eq 'RIFF' -and [Text.Encoding]::ASCII.GetString($header, 8, 4) -eq 'WEBP') {
        return 'image/webp'
    }
    if ($length -ge 6) {
        $signature = [Text.Encoding]::ASCII.GetString($header, 0, 6)
        if ($signature -eq 'GIF87a' -or $signature -eq 'GIF89a') {
            return 'image/gif'
        }
    }
    if ($length -ge 2 -and [Text.Encoding]::ASCII.GetString($header, 0, 2) -eq 'BM') {
        return 'image/bmp'
    }
    return $null
}

function Assert-ImageMime {
    param(
        [Parameter(Mandatory)]
        [string]$Path,

        [Parameter(Mandatory)]
        [string]$ExpectedMime
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "缺少旧媒体文件：$Path"
    }
    $actualMime = Get-ImageMime -Path $Path
    if ($actualMime -ne $ExpectedMime) {
        throw "媒体实际 MIME 不匹配：$Path（期望 $ExpectedMime，实际 $actualMime）"
    }
}

function New-QuarantineOutputPath {
    param(
        [Parameter(Mandatory)]
        [string]$Root,

        [Parameter(Mandatory)]
        [string]$RelativePath
    )

    $destination = Join-Path $Root $RelativePath
    if (Test-Path -LiteralPath $destination) {
        throw "拒绝覆盖已有隔离文件：$destination"
    }
    [void](New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination))
    return $destination
}

function Get-ChatDimensions {
    param(
        [Parameter(Mandatory)]
        [int]$Width,

        [Parameter(Mandatory)]
        [int]$Height,

        [Parameter(Mandatory)]
        [int]$MaxEdge
    )

    $longestEdge = [Math]::Max($Width, $Height)
    $scale = [Math]::Min(1.0, [double]$MaxEdge / [double]$longestEdge)
    return [PSCustomObject]@{
        Width = [Math]::Max(1, [int][Math]::Round($Width * $scale))
        Height = [Math]::Max(1, [int][Math]::Round($Height * $scale))
    }
}

function Get-JpegCodec {
    $codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() |
        Where-Object { $_.MimeType -eq 'image/jpeg' } |
        Select-Object -First 1
    if ($null -eq $codec) {
        throw '当前 Windows 图像组件不支持 JPEG 编码'
    }
    return $codec
}

function New-JpegChatVariant {
    param(
        [Parameter(Mandatory)]
        [string]$Source,

        [Parameter(Mandatory)]
        [string]$Destination,

        [Parameter(Mandatory)]
        [int]$MaxEdge,

        [Parameter(Mandatory)]
        [int]$Quality
    )

    Assert-ImageMime -Path $Source -ExpectedMime 'image/jpeg'
    $sourceImage = [Drawing.Image]::FromFile($Source)
    try {
        $dimensions = Get-ChatDimensions -Width $sourceImage.Width -Height $sourceImage.Height -MaxEdge $MaxEdge
        $bitmap = [Drawing.Bitmap]::new($dimensions.Width, $dimensions.Height)
        try {
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.Clear([Drawing.Color]::Black)
                $graphics.CompositingQuality = [Drawing.Drawing2D.CompositingQuality]::HighQuality
                $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $graphics.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::HighQuality
                $graphics.DrawImage($sourceImage, 0, 0, $dimensions.Width, $dimensions.Height)

                $encoderParameters = [Drawing.Imaging.EncoderParameters]::new(1)
                try {
                    $qualityParameter = [Drawing.Imaging.EncoderParameter]::new([Drawing.Imaging.Encoder]::Quality, [int64]$Quality)
                    try {
                        $encoderParameters.Param[0] = $qualityParameter
                        $jpegCodec = Get-JpegCodec
                        $bitmap.Save($Destination, $jpegCodec, $encoderParameters)
                    } finally {
                        $qualityParameter.Dispose()
                    }
                } finally {
                    $encoderParameters.Dispose()
                }
            } finally {
                $graphics.Dispose()
            }
        } finally {
            $bitmap.Dispose()
        }
    } finally {
        $sourceImage.Dispose()
    }

    Assert-ImageMime -Path $Destination -ExpectedMime 'image/jpeg'
    return [PSCustomObject]@{
        kind = 'chat-jpeg'
        source = $Source
        output = $Destination
        width = $dimensions.Width
        height = $dimensions.Height
        sha256 = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Copy-CorrectedWebp {
    param(
        [Parameter(Mandatory)]
        [string]$Source,

        [Parameter(Mandatory)]
        [string]$Destination
    )

    Assert-ImageMime -Path $Source -ExpectedMime 'image/webp'
    Copy-Item -LiteralPath $Source -Destination $Destination
    Assert-ImageMime -Path $Destination -ExpectedMime 'image/webp'
    return [PSCustomObject]@{
        kind = 'corrected-webp-extension'
        source = $Source
        output = $Destination
        sha256 = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$LegacyMediaRoot = Resolve-ExistingDirectory -Path $LegacyMediaRoot -Label '旧媒体根目录'
$QuarantineRoot = Initialize-EmptyQuarantineRoot -Path $QuarantineRoot -LegacyRoot $LegacyMediaRoot

# 只处理设计文档已记录的三张 WebP 伪装 JPG，输出仍停留在隔离目录。
$webpCorrections = @(
    @{ Source = '武魂\独狼.jpg'; Output = 'format-corrections\wuhun\lone-wolf\portrait.webp' },
    @{ Source = '武魂\龙神血脉.jpg'; Output = 'format-corrections\wuhun\dragon-god-bloodline\portrait.webp' },
    @{ Source = '魂兽\史莱姆.jpg'; Output = 'format-corrections\soul-beasts\slime\battle.webp' }
)

# 聊天变体仅缩小、保留宽高比，不裁切、不加水印，也不覆盖原图。
$chatVariants = @(
    @{ Source = '魂环\白.jpg'; Output = 'chat\soul-rings\white.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\黄.jpg'; Output = 'chat\soul-rings\yellow.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\紫.jpg'; Output = 'chat\soul-rings\purple.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\黑.jpg'; Output = 'chat\soul-rings\black.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\红.jpg'; Output = 'chat\soul-rings\red.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\橙.jpg'; Output = 'chat\soul-rings\orange.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '魂环\金.jpg'; Output = 'chat\soul-rings\gold.jpg'; MaxEdge = $SoulRingMaxEdge },
    @{ Source = '2C4D40D3E408BC53447FED74372D8BBE.jpg'; Output = 'chat\maps\douluo-world-overview.jpg'; MaxEdge = $WorldMapMaxEdge }
)

$results = @()
foreach ($entry in $webpCorrections) {
    $source = Join-Path $LegacyMediaRoot $entry.Source
    $destination = New-QuarantineOutputPath -Root $QuarantineRoot -RelativePath $entry.Output
    $results += Copy-CorrectedWebp -Source $source -Destination $destination
}
foreach ($entry in $chatVariants) {
    $source = Join-Path $LegacyMediaRoot $entry.Source
    $destination = New-QuarantineOutputPath -Root $QuarantineRoot -RelativePath $entry.Output
    $results += New-JpegChatVariant -Source $source -Destination $destination -MaxEdge $entry.MaxEdge -Quality $JpegQuality
}

$results | ConvertTo-Json -Depth 3
