# User guide

<p align="center"><img src="images/main-window.png" alt="Predator Lite main window" width="462"></p>

- [The window](#the-window)
  - [Monitoring](#monitoring)
  - [Fans](#fans)
  - [Performance](#performance)
  - [Keyboard](#keyboard)
  - [System](#system)
  - [Status line](#status-line)
- [Modes in detail](#modes-in-detail)
- [Hardware buttons](#hardware-buttons)
- [Tray icon](#tray-icon)
- [How settings are kept](#how-settings-are-kept)
- [Command line](#command-line)
- [Troubleshooting](#troubleshooting)

Controls for anything your laptop's firmware doesn't support are greyed out.

## The window

### Monitoring

- **CPU, GPU and System**: temperatures in °C.
- **CPU fan, GPU fan, Fan 3**: speed in RPM and, after the comma, the fan's real **duty cycle**
  (how hard it is driven, 0–100 %). On the PT515-51, "GPU fan" and "Fan 3" are the two fans on
  the GPU side.

Readings refresh every 2 seconds while the window is visible. When the window is hidden, nothing
is polled.

### Fans

- **Auto / Max / Custom**, separately for the CPU fan and the GPU fans.
  - *Auto*: the firmware's own curve.
  - *Max*: full speed.
  - *Custom*: the fixed duty set with the slider. The slider only acts while Custom is selected.
- **CoolBoost**: Acer's stronger Auto curve. It only matters while the fans are on Auto.

Choosing a fan mode by hand ends Turbo, Balanced or Eco, since those set the fans themselves.

All percentages are real duty cycles. The firmware's own "custom %" isn't (60 % runs the CPU fan
at about 35 %), so Predator Lite converts through a table measured on the PT515-51. The value in
Monitoring shows what the fan really does.

### Performance

- **Turbo / Balanced / Eco**: performance modes, at most one at a time. Click the lit one again to
  turn it off. See [Modes in detail](#modes-in-detail).
- **GPU OC**: NVIDIA overclock level, Normal, Faster or Turbo. The firmware supplies the clock
  offsets.
  - Choosing *Turbo* here switches Turbo mode on: the firmware treats the two as the same thing.
  - Outside Turbo mode the GPU tops out at Faster.
  - Overclocking pauses on battery (see [Battery rule](#battery-rule)).
- **LCD overdrive**: faster pixel response on the panel, less ghosting.
- **GPU mode**:
  - *Hybrid (Optimus)*: the Intel GPU drives the screen and the NVIDIA GPU works on demand.
    Longer battery life.
  - *Discrete only*: the NVIDIA GPU drives the screen directly. Best game performance.

  The firmware applies the change at the next restart. Predator Lite asks first: switch and
  restart now, switch and restart later, or cancel and keep the current mode. The note next to the box shows the running mode, or "Restart to switch
  to …" while a change is pending.

### Keyboard

- **Effect**:
  - *Static*: one color per zone. Click a **Zone** button to pick each. The window shows the
    zones the keyboard has (up to 4; the PT515-51 has 3).
  - *Breathing*, *Neon*, *Wave*, *Shifting*, *Zoom*: firmware animations.
- **Direction**: for Wave and Zoom.
- **Brightness**: 5 steps. **Speed**: 1–9, for the animated effects.
- **Effect color**: for Breathing, Neon and Zoom.
- **Win key**: unchecked locks the Windows and Menu keys, so a stray press can't throw you out of
  a game. This is the firmware's own lock. If your Win key ever stops working, check this box
  first.
- **Backlight off when idle**: turns the keyboard backlight off after 30 s, 1 min or 2 min
  without typing, or *Never*. It comes back on at the next key press.

### System

- **Boot animation and sound**: the Predator logo animation and sound at power-on.
- **USB charging when off**: keeps the USB ports powered while the laptop is off, for charging a
  phone. It stops when the battery falls to the chosen level (10, 20 or 30 %), so
  there is always charge left to start the laptop. *Off* disables it.

### Status line

The bottom line confirms each change ("Win key on", "GPU mode Discrete only after the next
restart", …) or shows an error. While Balanced or Eco runs, it shows what the fan curve is doing:
`Fans: CPU 55% (85 °C), GPU 19% (72 °C)`. These are the duty each fan group was given and the
smoothed temperature driving it.

## Modes in detail

### Turbo

- **What it does:** Turbo LED on, the firmware's turbo flag set, all fans at Max, GPU overclock at
  its Turbo level.
- **Turning it off** restores the fan modes, custom speeds and GPU level you had before.
- **The physical Turbo button** does the same, see [Hardware buttons](#hardware-buttons).
- **It needs AC power**, see the [Battery rule](#battery-rule).

### Balanced

Full performance: CPU boost and your GPU level stay. The fans follow a quiet software curve:

| Temperature | Fan duty |
|---|---|
| below 60 °C | off |
| 60 °C | 10 % |
| 70 °C | 16 % |
| 80 °C | 32 % |
| 87 °C | 62 % |
| 92 °C and above | 100 % |

### Eco

Lower power and less heat:
- **CPU:** Turbo Boost is turned off through a power plan named **Predator Lite Eco**. It is a
  copy of your plan with only that setting changed, so your own plan is never modified. When Eco
  ends, your plan is active again. If the app crashes while Eco is on, the Eco plan stays active
  until Predator Lite starts again, which re-applies Eco if it is still saved as on, or puts your
  plan back if not.
- **GPU:** the overclock goes to Normal, and the GPU clock is capped at 1000 MHz through
  `nvidia-smi` (configurable, see [settings file](settings-file.md)).
- **Fans:**

| Temperature | Fan duty |
|---|---|
| below 65 °C | off |
| 65 °C | 7 % |
| 75 °C | 16 % |
| 85 °C | 35 % |
| 90 °C and above | 100 % |

### How the fan curves behave

- **Each fan group follows its own chip:** the CPU fan follows the CPU temperature, the GPU fans
  follow the GPU temperature.
- **Short spikes are ignored.** Temperatures are smoothed over about 8 seconds, so starting an app
  doesn't spin the fans up. At 90 °C and above the raw temperature is used, with no delay.
- **Speeds drop gradually,** at most 10 % per 2 seconds. Small decreases are ignored.
- **Fans stop only 3 °C below their start temperature,** so they don't flap on and off.
- **If a temperature can't be read,** the fans go back to the firmware's Auto until it can.
- **Exiting the app** from Balanced or Eco returns the fans to Auto. The mode comes back at the
  next start.

The curves can be edited in the [settings file](settings-file.md#fan-curves).

### Battery rule

Like PredatorSense:
- **On battery or below 30 %:** overclocking and Turbo stop. The GPU goes to Normal and Turbo
  turns off.
- **On AC with at least 40 %:** overclocking comes back.

The status line, or a tray notification if the window is hidden, says when this happens.

## Hardware buttons

- **Turbo button**: the firmware handles this button itself. It flips its turbo flag and GPU
  level and tells Windows nothing. Predator Lite checks the flag once a second (no measurable
  CPU) and adds the rest: the Turbo LED and the fans at Max. You can switch this off with
  **Watch Turbo button** in the tray menu (shown on laptops whose firmware has the turbo flag).
- **PredatorSense button**: opens the window, or hides it if it is already in front. To make it
  start Predator Lite when the app is closed, see [PredatorSense button](predatorsense-button.md).

## Tray icon

- **Left-click**: show the window.
- **Right-click**: menu with
  - **Show**
  - **Turbo**, **Balanced**, **Eco** (with a check mark on the active one)
  - **Watch Turbo button** (on laptops whose firmware has the turbo flag)
  - **Exit**

<p align="center"><img src="images/tray-menu.png" alt="Tray menu" width="206"></p>

Closing the window only hides it. Use **Exit** to quit.

## How settings are kept

- **Where:** your choices are saved to `%APPDATA%\PredatorLite\settings.ini` on every change.
  See the [settings file reference](settings-file.md).
- **When they're applied:** at every start and after the laptop wakes from sleep, because the
  firmware may reset fans and lighting.
- **No settings file yet:** the app only reads the hardware and changes nothing.
- **Settings the firmware keeps itself:** LCD overdrive, boot animation and USB charging. These
  can also be changed in BIOS setup or Acer Care Center, so the app reads them from the firmware
  at start and after sleep instead of overwriting them. They are only written when you change
  them in the window.

## Command line

```
predator-lite.exe                  open the window (and the tray icon)
predator-lite.exe --hidden         start in the tray only
predator-lite.exe --toggle         show or hide a running instance, or start with the window open
predator-lite.exe --status FILE    write a read-only hardware report to FILE
```

Only one instance runs at a time. Starting it again brings the running one to the front.

`--status` is safe to run any time. It only reads: detected features, all sensors, fan modes, the
raw firmware values. Run it from a command prompt in the Predator Lite folder and accept the
administrator prompt. The prompt returns at once; the report appears in FILE a moment later.
Attach it when reporting a problem or a new laptop model.

## Troubleshooting

### Debug log

Create an empty file named `debug` (no extension) in `%APPDATA%\PredatorLite` and restart Predator
Lite. It then writes button events and decisions to `debug.log` in the same folder. Delete
`debug` to stop.

### Common problems

- **The Windows key does nothing.** The firmware's Win key lock is on. Check **Win key** in the
  Keyboard section.
- **"Access denied" or nothing works.** The app must run as administrator. Accept the UAC prompt,
  or use the [start at logon](installation.md#start-at-logon) task.
- **Fans change back by themselves.** Another fan tool (PredatorSense, NitroSense, FanControl, …)
  is running too. Close it; see
  [Coming from PredatorSense](installation.md#coming-from-predatorsense).
- **Eco says "GPU clock cap not applied".** `nvidia-smi` is missing or the driver refused. The
  CPU and fan parts of Eco still work.
- **GPU mode doesn't change.** It only switches at the next restart. The note next to the box
  says when a change is pending.
- **A control is greyed out.** Your laptop's firmware doesn't offer that feature (it was probed
  at start). `--status` shows what was detected.
- **The tray icon is missing.** Windows may have hidden it in the overflow area (the `^` arrow
  next to the clock). If Explorer restarted, the icon is added again automatically.
