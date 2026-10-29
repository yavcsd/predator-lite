#Requires -RunAsAdministrator
<#
.SYNOPSIS
Undo install-predatorsense-button.ps1.

.DESCRIPTION
Removes the button remap (other keyboard remaps are kept), sets Launch App 1 back to the Windows
default if it still points to the Predator Lite shortcut, and deletes the shortcut and the
"Predator Lite (button)" task. Restart Windows afterwards to restore the original key.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$kept = @(Get-ScancodeEntries | Where-Object { ($_ -shr 16) -ne $ButtonScanCode })
Set-ScancodeEntries $kept
Write-Host ("remap:     removed ({0} kept)" -f (Format-Count $kept.Count 'other entry' 'other entries'))

if (Test-Path $LaunchApp1Key) {
    $target = (Get-ItemProperty $LaunchApp1Key -Name ShellExecute -ErrorAction SilentlyContinue).ShellExecute
    if ($target -eq $ButtonShortcut) {
        Remove-Item $LaunchApp1Key -Recurse
        Write-Host 'app key:   Launch App 1 back to the Windows default'
    } else {
        Write-Host "app key:   left alone (points elsewhere: $target)"
    }
}
if (Test-Path $ButtonShortcut) { Remove-Item $ButtonShortcut; Write-Host 'shortcut:  removed' }
# The shortcut's folder, only if nothing else is left in it.
$shortcutDir = Split-Path $ButtonShortcut
if ((Test-Path $shortcutDir) -and -not (Get-ChildItem $shortcutDir -Force)) { Remove-Item $shortcutDir }
if (Get-ScheduledTask -TaskName $ButtonTaskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $ButtonTaskName -Confirm:$false
    Write-Host 'task:      removed'
}
Write-Host ''
Write-Host 'Done. Restart Windows to restore the original key.'
