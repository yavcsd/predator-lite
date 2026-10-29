# Settings file

Predator Lite keeps your choices in **`%APPDATA%\PredatorLite\settings.ini`**, as plain `key=value`
lines. The window writes it on every change, so you rarely need to touch it. Editing by hand is
useful for the fan curves and the Eco GPU cap, which have no controls in the window.

To edit it:
1. Exit Predator Lite (tray icon > Exit). The app would overwrite your edits on its next change.
2. Edit the file.
3. Start Predator Lite again.

Parsing is forgiving:
- Unknown keys and invalid values are ignored, and the default is used instead.
- Lines starting with `#`, `;` or `[` are comments.
- Keys written by pre-release builds are still understood.

If the file is missing, Predator Lite shows the firmware's current state and changes nothing
until you change something.

## Example

```ini
[predator-lite]
fan_units=duty
cpu_fan=auto
gpu_fan=auto
cpu_pct=50
gpu_pct=50
coolboost=0
gpu_oc=1
turbo=0
turbo_button=1
balanced=1
balanced_curve=60:10,70:16,80:32,87:62,92:100
eco=0
eco_curve=65:7,75:16,85:35,90:100
eco_gpu_mhz=1000
overdrive=1
boot_animation=0
usb_charge=30
win_key=1
kb_timeout=120
kb_restore=1
kb_effect=0
kb_speed=4
kb_brightness=100
kb_direction=1
kb_color=#00AEC7
zone1=#0000FF
zone2=#0080FF
zone3=#8000FF
zone4=#00AEC7
```

## Keys

### Fans

| Key | Values | Default | Meaning |
|---|---|---|---|
| `fan_units` | `duty` | `duty` | Marks fan percentages as real duty cycles. Files without it are from versions that stored the firmware's own values; those are converted when read, so the fans sound the same. |
| `cpu_fan`, `gpu_fan` | `auto`, `max`, `custom` | `auto` | Fan mode of the CPU fan / the GPU fans (outside Turbo, Balanced and Eco). |
| `cpu_pct`, `gpu_pct` | 0–100 | 50 | Custom speed, real duty in %. |
| `coolboost` | 0, 1 | 0 | Acer's stronger Auto fan curve. |

### Performance

| Key | Values | Default | Meaning |
|---|---|---|---|
| `gpu_oc` | 0 Normal, 1 Faster, 2 Turbo | 0 | GPU overclock outside Turbo mode. Turbo is capped to Faster outside Turbo mode, because the firmware treats GPU level Turbo as Turbo mode. |
| `turbo` | 0, 1 | 0 | Turbo mode. Not restored on battery. |
| `turbo_button` | 0, 1 | 1 | Watch the physical Turbo button (tray menu > Watch Turbo button). |
| `balanced` | 0, 1 | 0 | Balanced mode. |
| `balanced_curve` | curve | see below | Balanced fan curve. |
| `eco` | 0, 1 | 0 | Eco mode. |
| `eco_curve` | curve | see below | Eco fan curve. |
| `eco_gpu_mhz` | 0, or 300–3000 | 1000 | GPU clock cap in Eco mode (`nvidia-smi -lgc`). 0 means no cap. |

At most one of `turbo`, `balanced` and `eco` should be 1. If several are, Turbo wins over Eco,
and Eco over Balanced.

### Display and system

These three are kept by the firmware itself. Predator Lite reads them from the firmware at start,
so the values here are only a record of the last state.

| Key | Values | Default | Meaning |
|---|---|---|---|
| `overdrive` | 0, 1 | 1 | LCD overdrive. |
| `boot_animation` | 0, 1 | 1 | Boot animation and sound. |
| `usb_charge` | 0, 10, 20, 30 | 0 | USB charging when off: 0 = off, otherwise the battery level at which it stops. |

### Keyboard

| Key | Values | Default | Meaning |
|---|---|---|---|
| `win_key` | 0, 1 | 1 | 1 = Windows and Menu keys work, 0 = locked by the firmware. |
| `kb_timeout` | 0–255 seconds | 120 | Backlight off after this long without typing; 0 = never. The window offers 0, 30, 60 and 120. |
| `kb_restore` | 0, 1 | 1 | Restore the keyboard lighting below at start and after sleep. A first start writes 0, because the current effect can't be read; it becomes 1 when you change the lighting in the window. |
| `kb_effect` | 0 Static, 1 Breathing, 2 Neon, 3 Wave, 4 Shifting, 5 Zoom | 0 | Lighting effect. |
| `kb_speed` | 1–9 | 4 | Effect speed. |
| `kb_brightness` | 0–100 | 100 | Backlight brightness. The window uses steps of 25. |
| `kb_direction` | 1 right to left, 2 left to right | 1 | Direction for Wave and Zoom. |
| `kb_color` | `#RRGGBB` | `#00AEC7` | Color for Breathing, Neon and Zoom. |
| `zone1` … `zone4` | `#RRGGBB` | `#00AEC7` | Static color per zone (the PT515-51 has three). |

## Fan curves

A curve is a list of `temperature:duty` points, e.g. `60:10,70:16,80:32,87:62,92:100`.

- Below the first point's temperature, the fans are **off**.
- Between points, the duty rises linearly.
- From the last point on, the fans run at that point's duty.

A curve is accepted only if all of these hold. Otherwise the default is used, so a typo can never
leave the fans too slow:
- 1 to 8 points;
- temperatures between 40 and 95 °C, strictly rising;
- duties between 0 and 100, never falling;
- **the last point is 100 %**, so the fans always reach full speed by 95 °C at the latest.

Defaults:

| Mode | Curve |
|---|---|
| Balanced | `60:10,70:16,80:32,87:62,92:100` |
| Eco | `65:7,75:16,85:35,90:100` |

The CPU fan follows the CPU temperature and the GPU fans follow the GPU temperature, both with
the smoothing and hysteresis described in the
[user guide](user-guide.md#how-the-fan-curves-behave).

## Keys from pre-release builds

| Old key | Read as |
|---|---|
| `silent`, `silent_curve` | `balanced`, `balanced_curve` |
| `quiet`, `quiet_curve`, `quiet_gpu_mhz` | `eco`, `eco_curve`, `eco_gpu_mhz` |
| `lcd_overdrive` | `boot_animation`. Old versions wrote the boot animation setting under this name by mistake. |

They are written back under the new names at the next save.
