#Requires -RunAsAdministrator
<#
.SYNOPSIS
Start Predator Lite in the tray at every logon.

.DESCRIPTION
Registers a scheduled task "Predator Lite" that runs "predator-lite.exe --hidden" at logon with
administrator rights (no UAC prompt). It may start on battery, keeps running when unplugged and
has no time limit. Keeping the app running is what makes the Turbo button's fans and LED, the
fan curves and the settings restore after sleep work. Undo with uninstall-autostart.ps1.

.PARAMETER Exe
Path to predator-lite.exe. By default it is looked up next to this folder (release download)
or in ..\target\release (built from source).
#>
param([string]$Exe)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$Exe = Find-PredatorLite $Exe
$action    = New-ScheduledTaskAction -Execute $Exe -Argument '--hidden'
$trigger   = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
Register-ScheduledTask -TaskName $AutostartTaskName -Action $action -Trigger $trigger -Principal $principal `
    -Settings (New-PredatorLiteTaskSettings) -Force | Out-Null
Write-Host "task:      '$AutostartTaskName' -> $Exe --hidden at logon"
Write-Host ''
Write-Host 'Done. Predator Lite will start in the tray at your next logon.'
