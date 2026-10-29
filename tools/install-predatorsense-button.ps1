#Requires -RunAsAdministrator
<#
.SYNOPSIS
Make the PredatorSense button open Predator Lite, even when the app is not running.

.DESCRIPTION
  1. A scheduled task "Predator Lite (button)" that runs "predator-lite.exe --toggle" with
     administrator rights, so a button press never shows a UAC prompt.
  2. A shortcut that starts that task through a hidden console (nothing flashes on screen).
  3. "Launch App 1" (a standard Windows key) is set to open that shortcut, for this user only.
  4. A keyboard remap (Scancode Map) turns the PredatorSense button (E0 75) into Launch App 1
     (E0 6B). Other remaps you already have are kept. This part needs a reboot.

A running Predator Lite also listens for the button itself, so pressing it shows or hides the
window. Undo everything with uninstall-predatorsense-button.ps1.

.PARAMETER Exe
Path to predator-lite.exe. By default it is looked up next to this folder (release download)
or in ..\target\release (built from source).
#>
param([string]$Exe)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$Exe = Find-PredatorLite $Exe

# 1. On-demand task. Parallel instances, so a press also reaches an app that is already running.
$action    = New-ScheduledTaskAction -Execute $Exe -Argument '--toggle'
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
Register-ScheduledTask -TaskName $ButtonTaskName -Action $action -Principal $principal `
    -Settings (New-PredatorLiteTaskSettings -Parallel) -Force | Out-Null
Write-Host "task:      '$ButtonTaskName' -> $Exe --toggle"

# 2. Shortcut that starts the task without a visible console.
New-Item -ItemType Directory -Force (Split-Path $ButtonShortcut) | Out-Null
$sc = (New-Object -ComObject WScript.Shell).CreateShortcut($ButtonShortcut)
$sc.TargetPath  = "$env:SystemRoot\System32\conhost.exe"
$sc.Arguments   = "--headless `"$env:SystemRoot\System32\schtasks.exe`" /run /tn `"$ButtonTaskName`""
$sc.WindowStyle = 7
$sc.Description = 'Start Predator Lite, or show/hide its window'
$sc.Save()
Write-Host "shortcut:  $ButtonShortcut"

# 3. Launch App 1 -> shortcut (this user only; the machine-wide default stays untouched).
New-Item -Force $LaunchApp1Key | Out-Null
Set-ItemProperty $LaunchApp1Key -Name ShellExecute -Value $ButtonShortcut
Write-Host "app key:   Launch App 1 -> shortcut"

# 4. Key remap, merged into any existing map.
$entries = @(Get-ScancodeEntries | Where-Object { ($_ -shr 16) -ne $ButtonScanCode })
$entries += $ButtonRemapEntry
Set-ScancodeEntries $entries
Write-Host ("remap:     E0 75 -> E0 6B ({0} in Scancode Map)" -f (Format-Count $entries.Count 'entry' 'entries'))
Write-Host ''
Write-Host 'Done. Restart Windows to activate the key remap.'
