# Build a test installer, or sign both binaries and the installer using an
# externally provisioned code-signing certificate in the current user's store.
[CmdletBinding()]
param(
    [string]$BinDir = 'target/release',
    [string]$OutDir = 'dist',
    [switch]$Sign
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Checked([string]$Tool, [string[]]$Arguments) {
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Tool failed ($LASTEXITCODE)" }
}

Push-Location (Join-Path $PSScriptRoot '../..')
try {
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    if ($env:GITHUB_REF_TYPE -eq 'tag' -and $env:GITHUB_REF_NAME -ne "v$version") { throw 'Tag must match Cargo.toml version' }
    $BinDir = (Resolve-Path -LiteralPath $BinDir).Path
    $files = @('rterm.exe', 'rterm-agent.exe') | ForEach-Object {
        (Resolve-Path -LiteralPath (Join-Path $BinDir $_)).Path
    }
    $notices = Get-Item -LiteralPath 'target/release-notices/THIRD-PARTY-LICENSES.html'
    if ($notices.PSIsContainer -or $notices.Length -eq 0) { throw 'Generate third-party license notices before packaging (see docs/USAGE.md).' }
    if ($Sign) {
        if ($env:RTERM_WIN_CERT_THUMBPRINT -notmatch '^[a-fA-F0-9]{40}$') {
            throw 'Set RTERM_WIN_CERT_THUMBPRINT to an installed code-signing certificate thumbprint'
        }
        $signtool = (Get-Command signtool.exe -ErrorAction Stop).Source
        $signArgs = @('sign', '/sha1', $env:RTERM_WIN_CERT_THUMBPRINT, '/fd', 'SHA256', '/tr', 'http://timestamp.digicert.com', '/td', 'SHA256')
    } else {
        Write-Warning 'Creating an unsigned Windows test installer.'
    }
    function Sign-File([string]$Path) {
        Invoke-Checked $signtool ($signArgs + @($Path))
        Confirm-Signature $Path
    }
    function Confirm-Signature([string]$Path) {
        Invoke-Checked $signtool @('verify', '/pa', '/all', $Path)
    }
    # Sign copies, never modify the Cargo build outputs.
    $tempRoot = (Resolve-Path -LiteralPath ([IO.Path]::GetTempPath())).Path
    $stage = Join-Path $tempRoot ("rterm-package-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    try {
        foreach ($file in $files) {
            Copy-Item -LiteralPath $file -Destination $stage
            if ($Sign) { Sign-File (Join-Path $stage ([IO.Path]::GetFileName($file))) }
        }
        $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe'
        if (-not (Test-Path -LiteralPath $iscc)) { throw 'Install Inno Setup 6 first' }
        $compilerArgs = @("/DAppVersion=$version", "/DBinDir=$stage", "/DOutDir=$stage")
        if ($Sign) {
            # Inno signs Setup and its embedded uninstaller during compilation.
            # $q and $f are Inno substitutions, not PowerShell variables.
            $command = '/SRTermSign=$q' + $signtool.Replace('$', '$$') + '$q ' + ($signArgs -join ' ') + ' $f'
            $compilerArgs += @('/DSignedBuild=1', $command)
        }
        Invoke-Checked $iscc ($compilerArgs + @('packaging/windows/rterm.iss'))
        $name = "RTerm-$version-windows-x64-setup.exe"
        $installer = Join-Path $stage $name
        if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'Installer was not created' }
        if ($Sign) { Confirm-Signature $installer }
        New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
        $destination = Join-Path $OutDir $name
        Copy-Item -LiteralPath $installer -Destination $destination -Force
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant()
        "$hash  $name" | Set-Content -Encoding ascii -Path "$destination.sha256"
    } finally {
        $resolvedStage = (Resolve-Path -LiteralPath $stage).Path
        if ((Split-Path -Parent $resolvedStage) -ne $tempRoot.TrimEnd('\', '/') -or (Split-Path -Leaf $resolvedStage) -notmatch '^rterm-package-[a-f0-9]{32}$') {
            throw 'Refusing to remove an unexpected packaging directory'
        }
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
} finally {
    Pop-Location
}
