param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Binary not found: $BinaryPath"
}

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$dumpbin = $null
if (Test-Path -LiteralPath $vswhere) {
    $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($installPath) {
        $dumpbin = Get-ChildItem -Path (Join-Path $installPath "VC\Tools\MSVC") -Filter "dumpbin.exe" -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "Hostx64\\x64\\dumpbin\.exe$" } |
            Select-Object -First 1 -ExpandProperty FullName
    }
}
if (-not $dumpbin) {
    $cmd = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($cmd) { $dumpbin = $cmd.Source }
}
if (-not $dumpbin) {
    throw "Could not locate dumpbin.exe to inspect PE imports"
}

$output = & $dumpbin /nologo /dependents $BinaryPath
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin failed with exit code $LASTEXITCODE"
}

Write-Host "PE dependents for ${BinaryPath}:"
$output | ForEach-Object { Write-Host $_ }

$forbidden = $output |
    ForEach-Object { $_.Trim() } |
    Where-Object { $_ -match '(?i)^(vcruntime\d*(_\d+)?|msvcp\d*(_\d+)?|concrt\d*|ucrtbase|api-ms-win-crt-.*)\.dll$' }

if ($forbidden) {
    throw "Windows binary dynamically imports forbidden MSVC/CRT DLL(s): $($forbidden -join ', ')"
}

Write-Host "OK: no dynamic MSVC/CRT runtime DLL imports found."
