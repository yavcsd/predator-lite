# Installation

- [Requirements](#requirements)
- [Install](#install)
- [Coming from PredatorSense](#coming-from-predatorsense)
- [First start](#first-start)
- [Start at logon](#start-at-logon)
- [Running the setup scripts](#running-the-setup-scripts)
- [Updating](#updating)
- [Uninstalling](#uninstalling)

## Requirements

- **Windows 10 or 11, 64-bit.**
- **An Acer laptop whose BIOS offers Acer's gaming WMI interface.** That is most Predator, Helios,
  Triton and Nitro models from about 2018 to 2021. Newer models are not supported yet. See
  [Supported laptops](../README.md#supported-laptops).
- **Windows 10 version 1809 or later** for the optional PredatorSense button setup (it starts the
  app through `conhost --headless`).
- **Administrator rights.** The BIOS methods only answer administrators, so the app asks for
  elevation when it starts.
- **Optional:** the NVIDIA driver, which installs `nvidia-smi`. Without it, Eco mode still works
  but can't cap the GPU clock.

Nothing else is needed: no .NET, no Visual C++ redistributable, no driver.

## Install

1. Download `predator-lite.zip` from the
   [Releases page](https://github.com/yavcsd/predator-lite/releases).
2. Unzip it into a folder that will stay put, for example `C:\Tools\Predator Lite`. The optional
   start-at-logon and button setups point to `predator-lite.exe` in that folder, so they stop
   working if you move it.

The folder holds:

| File | What it is |
|---|---|
| `predator-lite.exe` | The program |
| `README.md`, `LICENSE` | Overview and license. The full guides are on the [project page](https://github.com/yavcsd/predator-lite#guides). |
| `tools\install-autostart.ps1`, `tools\uninstall-autostart.ps1` | [Start at logon](#start-at-logon) |
| `tools\install-predatorsense-button.ps1`, `tools\uninstall-predatorsense-button.ps1` | [PredatorSense button setup](predatorsense-button.md) |
| `tools\common.ps1` | Shared by the scripts above |

If you'd rather build it yourself, see [Building from source](../README.md#building-from-source).
The setup scripts then find the program in `target\release` automatically.

## Coming from PredatorSense

PredatorSense and Predator Lite both control the same fans, LEDs and settings, and running both
makes them fight. Its background service can, for example, put its own fan mode back after you
change it. Either:

- **Uninstall PredatorSense** (Settings > Apps > Installed apps > PredatorSense). This is the
  cleanest option. Remove any separately listed PredatorSense service package too.
- **Or keep it but don't run it.** Exit PredatorSense, then in `services.msc` find its service
  (its program is `PSSvc.exe`; the display name varies by version) and set it to *Disabled*.

Your firmware settings (keyboard colors, GPU mode, …) stay where PredatorSense left them.
Predator Lite reads them at its first start. The keyboard effect can't be read, so the lighting
is left untouched until you change it: see [First start](#first-start).

## First start

Double-click `predator-lite.exe` and accept the administrator prompt. The program isn't
code-signed, so the first time Windows SmartScreen may show "Windows protected your PC": click
**More info**, then **Run anyway**.

- The window opens and a Predator icon appears in the notification area (tray). Windows may put
  it in the hidden-icons overflow (the `^` arrow). Drag it next to the clock to keep it visible.
- **The first start only reads.** It shows what the firmware currently has and changes nothing.
  From your first change on, your choices are saved to `%APPDATA%\PredatorLite\settings.ini` and
  applied again at every start and after sleep.
- **Keyboard effect:** the firmware reports the zone colors but not the current effect, so the
  window shows **Static** even if PredatorSense left an animated effect (Wave, Breathing, …).
  Predator Lite leaves the keyboard lighting exactly as it is until you change the lighting
  yourself in the window. From then on, your lighting is restored at every start.
- Closing the window (×) hides it to the tray, so the hardware buttons and fan curves keep
  working. To quit, right-click the tray icon and choose **Exit**. Exiting from Balanced or Eco
  puts the fans back on the firmware's Auto and removes Eco's limits; your mode comes back at
  the next start. Other fan settings stay as they are, since the firmware keeps them.

Next: the [user guide](user-guide.md) walks through every control.

## Start at logon

Predator Lite has to be running for these to work:
- the Turbo button's fans and LED,
- the Balanced and Eco fan curves,
- restoring your settings after sleep.

To start it in the tray at every logon (without a UAC prompt), run this in an **administrator**
PowerShell in the Predator Lite folder (see [Running the setup scripts](#running-the-setup-scripts)):

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\install-autostart.ps1
```

This registers a scheduled task named **Predator Lite** that runs `predator-lite.exe --hidden`
at logon with administrator rights. It may start on battery, keeps running when unplugged, and
has no time limit, unlike Windows' defaults for scheduled tasks.

To undo it:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\uninstall-autostart.ps1
```

## Running the setup scripts

The scripts in `tools\` change system settings (scheduled tasks, a keyboard remap), so they must
run in an **administrator** PowerShell:

1. Open Start, type `PowerShell`, right-click **Windows PowerShell** and choose **Run as
   administrator**.
2. Go to the Predator Lite folder, e.g. `cd "C:\Tools\Predator Lite"`.
3. Run the script with `powershell -ExecutionPolicy Bypass -File .\tools\<script>.ps1`.
   `-ExecutionPolicy Bypass` applies to that one run only. It doesn't change your system's
   script policy.

If `predator-lite.exe` is somewhere else, pass its path: `... -File .\tools\install-autostart.ps1
-Exe "D:\Apps\predator-lite.exe"`.

## Updating

1. Right-click the tray icon and choose **Exit**.
2. Replace `predator-lite.exe` with the new one, in the same folder.
3. Start it again, or log off and on if you use start at logon.

Settings files from pre-release builds are read and converted automatically. That includes fan
percentages saved before the duty calibration and the old "silent" / "quiet" mode names.

## Uninstalling

1. Undo the optional setups, if you used them:
   ```powershell
   powershell -ExecutionPolicy Bypass -File .\tools\uninstall-autostart.ps1
   powershell -ExecutionPolicy Bypass -File .\tools\uninstall-predatorsense-button.ps1
   ```
   The button removal needs a restart to give the button back its original scan code.
2. Set both fan groups to **Auto** if they aren't, then right-click the tray icon and choose
   **Exit**. The firmware keeps the last fan mode after the app is gone.
3. Delete the Predator Lite folder and `%APPDATA%\PredatorLite`, which holds `settings.ini`,
   `eco_prev_plan` while Eco is on, and the optional debug log.
4. If you ever used Eco mode, remove its power plan.
   - **Control Panel:** Control Panel > Power Options, select another plan first, then delete
     **Predator Lite Eco**.
   - **Command line:** `powercfg /list` to find its GUID, then `powercfg /delete <GUID>`.

Firmware settings (keyboard colors, GPU mode, …) stay as you last set them. They live in the
laptop, not in Windows. The Win key is the exception: on the PT515-51 a full shutdown locks it
again, until something turns it back on.
