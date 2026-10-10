[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargo = if ($null -ne $cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path -LiteralPath $cargo -PathType Leaf)) { throw 'Rust cargo.exe was not found.' }
foreach ($required in @('Cargo.toml', 'Cargo.lock', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'docs\compatibility.md')) {
    if (-not (Test-Path -LiteralPath (Join-Path $projectRoot $required) -PathType Leaf)) { throw "Required package source is missing: $required" }
}
Push-Location $projectRoot
try {
    & $cargo fmt --check
    if ($LASTEXITCODE -ne 0) { throw 'Formatting check failed.' }
    & $cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    & $cargo clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed.' }
    & $cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    $executable = Join-Path $projectRoot 'target\release\caretlang.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Release executable is missing.' }
    $distribution = Join-Path $projectRoot 'dist\CaretLang'
    $docs = Join-Path $distribution 'docs'
    [IO.Directory]::CreateDirectory($docs) | Out-Null
    $destinationExe = Join-Path $distribution 'caretlang.exe'
    if (-not (Test-Path -LiteralPath $destinationExe) -or
        (Get-FileHash -LiteralPath $executable).Hash -ne (Get-FileHash -LiteralPath $destinationExe).Hash) {
        Copy-Item -LiteralPath $executable -Destination $destinationExe -Force
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination (Join-Path $distribution 'README.md') -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination (Join-Path $distribution 'LICENSE') -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'THIRD_PARTY_NOTICES.md') -Destination (Join-Path $distribution 'THIRD_PARTY_NOTICES.md') -Force
    Copy-Item -LiteralPath (Join-Path $projectRoot 'docs\compatibility.md') -Destination (Join-Path $docs 'compatibility.md') -Force
    $archive = Join-Path $projectRoot 'dist\CaretLang-portable.zip'
    # List only intended artifacts, so old unrelated distribution files cannot leak into the archive.
    Add-Type -AssemblyName System.IO.Compression
    $stream = [IO.File]::Open($archive, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($relative in @('caretlang.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'docs/compatibility.md')) {
                $entry = $zip.CreateEntry("CaretLang/$relative", [IO.Compression.CompressionLevel]::Optimal)
                $entryStream = $entry.Open()
                try {
                    $sourceStream = [IO.File]::OpenRead((Join-Path $distribution $relative))
                    try { $sourceStream.CopyTo($entryStream) } finally { $sourceStream.Dispose() }
                } finally { $entryStream.Dispose() }
            }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  CaretLang-portable.zip" | Set-Content -LiteralPath "$archive.sha256" -Encoding ascii
    Write-Output "Package: $archive"
    Write-Output "SHA256: $hash"
} finally { Pop-Location }
