[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$WixTools,
    [string]$AppIcon,
    [string]$ReleaseDir,
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$compiler = (Get-Item -LiteralPath (Join-Path $WixTools 'candle.exe')).FullName
$linker = (Get-Item -LiteralPath (Join-Path $WixTools 'light.exe')).FullName
$root = Split-Path $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$metadataText = & cargo metadata --no-deps --locked --format-version 1 --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
$metadata = $metadataText | ConvertFrom-Json
$package = $metadata.packages[0]
$version = $package.version

if ($ReleaseDir) {
    $stage = (Resolve-Path -LiteralPath $ReleaseDir).Path
    $executable = (Get-Item -LiteralPath (Join-Path $stage 'BTWN.exe')).FullName
    $guide = Join-Path $stage 'Quick Start.md'
    $license = Join-Path $stage 'LICENSE.txt'
    $notices = $stage
} else {
    $messages = & cargo build --locked --release --manifest-path $manifest --message-format=json-render-diagnostics
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
    foreach ($line in $messages) {
        $record = $line | ConvertFrom-Json
        if ($record.package_id -ne $package.id) { continue }
        if ($record.reason -eq 'compiler-artifact' -and $record.executable) {
            $executable = $record.executable
        }
        if ($record.reason -eq 'build-script-executed' -and !$AppIcon) {
            $AppIcon = Join-Path $record.out_dir 'btwn.ico'
        }
    }
    $guide = Join-Path $PSScriptRoot 'Quick Start.md'
    $license = Join-Path $root 'LICENSE.txt'
    $notices = Join-Path $PSScriptRoot 'notices'
}
if (!$AppIcon -or !(Test-Path -LiteralPath $AppIcon -PathType Leaf)) {
    throw 'The Windows GNU release build produces btwn.ico. Use that toolchain or specify -AppIcon.'
}
if (!$executable -or !(Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Missing BTWN executable.' }
if (!$Output) { $Output = Join-Path $metadata.target_directory "installer/BTWN-$version-Setup-x64.msi" }
$Output = [IO.Path]::GetFullPath($Output)
[void][IO.Directory]::CreateDirectory((Split-Path $Output))
$AppIcon = (Resolve-Path -LiteralPath $AppIcon).Path
$source = Join-Path $PSScriptRoot 'BTWN.wxs'
$object = Join-Path ([IO.Path]::GetTempPath()) (([Guid]::NewGuid().ToString('N')) + '.wixobj')
try {
    $defines = @("-dProductVersion=$version", "-dAppIcon=$AppIcon", "-dExeSource=$executable",
        "-dGuide=$guide", "-dLicense=$license", "-dNotices=$notices")
    & $compiler -nologo -arch x64 -ext WixFirewallExtension @defines -out $object $source
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed.' }
    & $linker -nologo -ext WixFirewallExtension -ext WixUIExtension -spdb -out $Output $object
    if ($LASTEXITCODE -ne 0) { throw 'Installer validation or linking failed.' }
    Get-Item -LiteralPath $Output
} finally {
    if (Test-Path -LiteralPath $object) { Remove-Item -LiteralPath $object }
}
