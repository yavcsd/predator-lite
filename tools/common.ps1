# Shared names and helpers for the setup scripts in this folder. Not meant to be run directly.

# Scheduled tasks created by the scripts.
$ButtonTaskName    = 'Predator Lite (button)'   # on demand, started by the PredatorSense button
$AutostartTaskName = 'Predator Lite'            # at logon, starts the app in the tray

# The PredatorSense button sends scan code E0 75. It is remapped to E0 6B, the standard
# "Launch App 1" key, which Windows can be told to open a shortcut for.
$ButtonShortcut = Join-Path $env:LOCALAPPDATA 'PredatorLite\PredatorSense button.lnk'
$LaunchApp1Key  = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\AppKey\17'
$KeyboardLayout = 'HKLM:\SYSTEM\CurrentControlSet\Control\Keyboard Layout'
$ButtonScanCode = 0xE075
# Scancode Map entry: high word = key sent by the keyboard, low word = key Windows sees instead.
# Built from a string because PowerShell reads the literal 0xE075E06B as a negative Int32.
$ButtonRemapEntry = [Convert]::ToUInt32('E075E06B', 16)

# Find predator-lite.exe: next to this folder (release download), or the cargo build output.
function Find-PredatorLite([string]$Exe) {
    if ($Exe) { return (Resolve-Path $Exe).Path }
    $candidates = @(
        (Join-Path $PSScriptRoot '..\predator-lite.exe'),
        (Join-Path $PSScriptRoot 'predator-lite.exe'),
        (Join-Path $PSScriptRoot '..\target\release\predator-lite.exe')
    )
    foreach ($c in $candidates) { if (Test-Path $c) { return (Resolve-Path $c).Path } }
    throw "predator-lite.exe not found. Pass its location: -Exe 'C:\path\to\predator-lite.exe'"
}

# Settings shared by both tasks: start on battery, keep running on battery, no time limit
# (Windows' defaults would stop the app after 72 hours or when unplugged).
function New-PredatorLiteTaskSettings([switch]$Parallel) {
    $options = @{ AllowStartIfOnBatteries = $true; DontStopIfGoingOnBatteries = $true; ExecutionTimeLimit = [TimeSpan]::Zero }
    if ($Parallel) { $options.MultipleInstances = 'Parallel' }
    New-ScheduledTaskSettingsSet @options
}

# Scancode Map layout: 8 zero bytes, uint32 count (entries + 1), the uint32 entries, uint32 0.
function Get-ScancodeEntries {
    $v = (Get-ItemProperty $KeyboardLayout -Name 'Scancode Map' -ErrorAction SilentlyContinue).'Scancode Map'
    if (-not $v -or $v.Length -lt 16) { return }
    $n = [BitConverter]::ToUInt32($v, 8)
    for ($i = 0; $i -lt $n - 1 -and 12 + 4 * $i + 4 -le $v.Length; $i++) {
        $e = [BitConverter]::ToUInt32($v, 12 + 4 * $i)
        if ($e -ne 0) { $e }
    }
}

function Set-ScancodeEntries([uint32[]]$Entries) {
    if (-not $Entries -or $Entries.Count -eq 0) {
        Remove-ItemProperty $KeyboardLayout -Name 'Scancode Map' -ErrorAction SilentlyContinue
        return
    }
    $b = New-Object byte[] (16 + 4 * $Entries.Count)
    [BitConverter]::GetBytes([uint32]($Entries.Count + 1)).CopyTo($b, 8)
    for ($i = 0; $i -lt $Entries.Count; $i++) { [BitConverter]::GetBytes($Entries[$i]).CopyTo($b, 12 + 4 * $i) }
    Set-ItemProperty $KeyboardLayout -Name 'Scancode Map' -Value $b -Type Binary
}

function Format-Count([int]$n, [string]$one, [string]$many) { if ($n -eq 1) { "$n $one" } else { "$n $many" } }
