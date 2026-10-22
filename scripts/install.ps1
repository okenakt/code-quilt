# Installs CodeQuilt into ~/.codequilt/bin, or removes it with -Uninstall. See docs/features.md.
#   irm https://github.com/okenakt/code-quilt/releases/latest/download/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://github.com/okenakt/code-quilt/releases/latest/download/install.ps1))) [options]
param(
    [string]$Version = 'latest',
    [string]$Archive,
    [switch]$Yes,
    [switch]$NoModifyPath,
    [switch]$Desktop,
    [switch]$Uninstall
)

# No `exit` below: under `iex` it would close the caller's shell.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'okenakt/code-quilt'
$Asset = 'codequilt-x86_64-pc-windows-msvc.zip'
$Root = Join-Path $HOME '.codequilt'
$Bin = Join-Path $Root 'bin'
$Bins = 'cq.exe', 'cq-orchestrator.exe', 'cq-board.exe'
$PathEntry = '%USERPROFILE%\.codequilt\bin'
$Shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'CodeQuilt.lnk'
$Cq = Join-Path $Bin 'cq.exe'

function Stop-Daemon {
    & $Cq daemon stop
    if ($LASTEXITCODE -ne 0) { Write-Warning 'could not stop the daemon' }
}

# Running exes cannot be deleted but can be renamed; the renamed copies go once nothing uses them.
function Remove-Locked([string]$File) {
    if (Test-Path $File) {
        Move-Item $File "$File.$([DateTime]::Now.Ticks).old"
    }
}

function Remove-Old {
    Get-ChildItem $Bin -Filter '*.old' -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
}

function Get-Download([string]$Dir) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 'Tls12'
    $base = if ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download" }
            else { "https://github.com/$Repo/releases/download/$Version" }
    Write-Host "downloading $base/$Asset"
    $zip = Join-Path $Dir $Asset
    Invoke-WebRequest -UseBasicParsing "$base/$Asset" -OutFile $zip
    $sums = (Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS").Content
    $sums = if ($sums -is [byte[]]) { [Text.Encoding]::UTF8.GetString($sums) } else { $sums }
    $line = $sums -split "`n" | Where-Object { $_ -match " $([regex]::Escape($Asset))\s*$" }
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash
    if (-not $line -or $hash -ne ($line -split ' ')[0]) { throw "checksum mismatch for $Asset" }
    $zip
}

# The user Path is read and written unexpanded so entries like %USERPROFILE% stay as they are.
function Get-UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try { $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames') } finally { $key.Close() }
}

function Test-UserPath {
    @((Get-UserPath) -split ';' | Where-Object { $_ -eq $PathEntry -or $_ -eq $Bin }).Count -gt 0
}

# Asks the user unless an option answered it. Without a console to ask on it takes the default.
function Confirm-ModifyPath {
    if ($NoModifyPath) { return $false }
    if ($Yes -or [Console]::IsInputRedirected) { return $true }
    Write-Host "Add $PathEntry to PATH:"
    Write-Host '  1) user Path  (default)'
    Write-Host '  2) do not modify'
    while ($true) {
        switch (Read-Host '>') {
            { $_ -in '', '1' } { return $true }
            '2' { return $false }
        }
    }
}

function Set-UserPath([scriptblock]$Change) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    try {
        $old = $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        $entries = @($old -split ';' | Where-Object { $_ -and $_ -ne $PathEntry -and $_ -ne $Bin })
        $new = (& $Change $entries) -join ';'
        if ($new -ne $old) { $key.SetValue('Path', $new, 'ExpandString') }
    } finally {
        $key.Close()
    }
    # Tell Explorer so new terminals see the change.
    if (-not ('CodeQuilt.Win32' -as [type])) {
        Add-Type -Namespace CodeQuilt -Name Win32 -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
'@
    }
    $r = [UIntPtr]::Zero
    [void][CodeQuilt.Win32]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$r)
}

function Install-CodeQuilt {
    # Asked once, before anything changes; a later run keeps the entry already there.
    $modifyPath = -not (Test-UserPath) -and (Confirm-ModifyPath)
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "codequilt-$([guid]::NewGuid())"
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        $zip = if ($Archive) { $Archive } else { Get-Download $tmp }
        $x = Join-Path $tmp 'x'
        Expand-Archive $zip $x
        foreach ($b in $Bins) {
            if (-not (Test-Path (Join-Path $x $b))) { throw "$Asset has no $b" }
        }

        # The cq that started the daemon speaks its protocol version, so it stops it.
        $updating = Test-Path $Cq
        if ($updating) { Stop-Daemon }
        New-Item -ItemType Directory -Force $Bin | Out-Null
        foreach ($b in $Bins) {
            $dest = Join-Path $Bin $b
            Remove-Locked $dest
            Copy-Item (Join-Path $x $b) $dest
        }
        Remove-Old
        Write-Host "installed $(& $Cq --version) into $Bin"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    if ($modifyPath) {
        Set-UserPath { param($entries) @($PathEntry) + $entries }
        if (($env:Path -split ';') -notcontains $Bin) { $env:Path = "$Bin;$env:Path" }
        Write-Host "added to the user Path: $PathEntry"
    }
    if ($Desktop) {
        $lnk = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
        $lnk.TargetPath = Join-Path $Bin 'cq-board.exe'
        $lnk.WorkingDirectory = $HOME
        $lnk.Save()
    }
    if ($updating) {
        & $Cq daemon restart
        if ($LASTEXITCODE -ne 0) { Write-Warning 'could not restart the daemon' }
    }
    if (Get-Process cq-board -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Bin\*" }) {
        Write-Host 'restart the board to use the new version'
    }
    if ($modifyPath) { Write-Host 'open a new terminal to use cq from other shells' }
    elseif (-not (Test-UserPath)) { Write-Host "$Bin is not on PATH; add it yourself to run cq by name" }
}

function Uninstall-CodeQuilt {
    if (Test-Path $Cq) { Stop-Daemon }
    foreach ($b in $Bins) { Remove-Locked (Join-Path $Bin $b) }
    Remove-Old
    $left = @(Get-ChildItem $Bin -ErrorAction SilentlyContinue)
    if ($left) {
        Write-Host "still in use, delete $Bin once they exit: $($left.Name -join ', ')"
    } else {
        Remove-Item $Bin -ErrorAction SilentlyContinue
    }
    Set-UserPath { param($entries) $entries }
    Remove-Item $Shortcut -ErrorAction SilentlyContinue
    Write-Host "removed CodeQuilt; sessions and settings are kept in $Root"
}

if ($Uninstall) { Uninstall-CodeQuilt } else { Install-CodeQuilt }
