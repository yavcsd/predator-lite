# Firmware interface

Everything Predator Lite does goes through WMI methods that the laptop's BIOS exposes to Windows
in the `ROOT\WMI` namespace. This page documents the calls as decoded and verified on a
**Predator Triton 500 (PT515-51), BIOS V1.13**. It is written for developers of Predator Lite and
of other tools. The encodings live in [`src/protocol.rs`](../src/protocol.rs).

How it was decoded:
- static analysis of Acer PredatorSense 3.0.3136 (`PSSvc.exe`, `PSAdminAgent.exe` and the UWP
  front end): strings, disassembly, .NET IL;
- a read-only probe of every `Get*` method on the laptop;
- supervised write tests with read-back, the state restored afterwards.

No Acer code is included in this project.

## Classes and calling convention

| Class | Methods | Used for |
|---|---|---|
| `AcerGamingFunction` | `Get/SetGamingSysInfo`, `…FanBehavior`, `…FanSpeed`, `…LED`, `…RgbKb`, `…KBBacklight`, `…MiscSetting`, `…Profile` | Sensors, fans, Turbo, keyboard, GPU, panel |
| `APGeAction` | `GetFunction(uiInput)`, `SetFunction(uiInput)` | CoolBoost, keyboard backlight timeout, USB charging when off |
| `APGeEvent` | event class (`EventDetail`) | Hotkey events (none fire for the buttons on the PT515-51) |
| `BatteryControl` | `GetBatteryHealthControlStatus`, `SetBatteryHealthControl`, `GetBattInfoInterface` | Battery health mode; **unsupported** on the PT515-51, temperature only |

- **Arguments:** each method takes one packed integer and returns one: `gmInput` / `gmOutput` on
  `AcerGamingFunction`, `uiInput` / `uiOutput` on `APGeAction`.
- **Types:** getters take `uint32` and return `uint64`. Most setters take `uint64` and return
  `uint32`. `SetGamingKBBacklight` takes a `uint8[16]`. WMI passes 64-bit integers as strings,
  which `src/wmi.rs` handles.
- **Status:** byte 0 of every output is a status code. `AcerGamingFunction` uses 0 = OK,
  1 = unsupported, 2 = invalid parameter. `APGeAction` uses `0xE1` / `0xE2` for the same. The
  payload is `output >> 8`.
- **Bit layout:** below, `x << n` means "x shifted left n bits", `|` is bitwise OR, and byte n of
  a value is bits `8n .. 8n+7`.

## Sensors: `GetGamingSysInfo`

Input `(id << 8) | 1`. The value is in bits 8..23 of the output.

| id | Reading |
|---|---|
| 1 | CPU temperature °C |
| 2 | CPU fan RPM |
| 3 | System temperature °C |
| 6 | GPU fan RPM |
| 9 | Fan 3 (second GPU-side fan) RPM |
| 10 | GPU temperature °C |
| 12 | CPU fan duty % |
| 16 | GPU fan duty % |
| 17 | Fan 3 duty % |

The duty ids 12, 16 and 17 are the ones PredatorSense reads next to each RPM. Input `0` returns a
supported-sensor bitmap in bits 24+, where bit n means id n+1. Input `2` returns, in byte 5, the
"battery boost" flag that PredatorSense checks before allowing overclocking.

## Fans

**Mode, `SetGamingFanBehavior`.** The input has a slot bitmap in bits 0..15 and a 2-bit mode per
slot at bit `16 + 2·slot`: 1 = Auto, 2 = Max, 3 = Custom.
- The PT515-51 has slots 0 (CPU), 3 and 4 (GPU side).
- Example: all three fans on Auto is `0x19 | 1<<16 | 1<<22 | 1<<24`.
- `GetGamingFanBehavior(1 << slot)` reads one slot back.

**Speed, `SetGamingFanSpeed`.** Input `fan id | percent << 8`, with fan id = slot + 1 (1 = CPU;
4 and 5 = GPU side; both GPU ids must be set). The percentage only applies in Custom mode.

**The firmware's percentage is not a duty cycle.** Measured with the duty sensors, settled values:

| Firmware % | 0 | 20 | 30 | 40 | 50 | 60 | 70 | 80 | 90 | 100 |
|---|---|---|---|---|---|---|---|---|---|---|
| CPU fan duty % | 0 (stopped) | 7 | 13 | 19 | 28 | 35 | 46 | 62 | 78 | 100 |
| GPU fans duty % | 0 | ~6 (estimated) | 11 | 16 | 22 | 31 | 41 | 55 | 74 | 100 |

The response is roughly quadratic, and 0 % stops the fan: it is not a boost over Auto. Predator
Lite converts the duty you ask for through this table (`protocol::firmware_fan_pct`). The fans
take about 6 seconds to settle after a change.

**CoolBoost** (`APGeAction`): get `0x207`, where byte 1 is the state; set `7 | on << 16`.
`GetFunction(0x0007)` describes function 7; byte 2 = 1 means CoolBoost.

## Turbo and GPU

| Setting | Call |
|---|---|
| Turbo LED | `SetGamingLED(0x10001)` on, `SetGamingLED(0x1)` off; `GetGamingLED(1)` bits 0–1 = state |
| Misc settings | `SetGamingMiscSetting(index \| value << 8)`, `GetGamingMiscSetting(index)` |

| Misc index | Meaning |
|---|---|
| 1 | Firmware turbo flag (0 / 1) |
| 2 | GPU mode (MUX): 1 = Hybrid, 2 = Discrete. Applies at the next restart. |
| 5 | GPU overclock level 0 / 1 / 2 (Normal / Faster / Turbo); reads back as `0x2L` |
| 6 | Boot animation and sound (0 / 1) |
| 7–16 | Invalid on the PT515-51. Other models use 7 = CPU overclock, 9/10 = GPU mode support, 0x0A/0x0B = platform profile, 0x0F = Fn lock. |

Two quirks:
- **The Turbo button is handled entirely by the firmware.** It flips misc 1 and misc 5 together
  (flag 1 ⟺ GPU level `0x22`) and sends Windows no WMI, keyboard or HID event.
- **GPU level Turbo is Turbo mode.** Writing a misc value the firmware already holds can act like
  a Turbo button press, so Predator Lite only writes values that differ.

## Profile word: `GetGamingProfile` / `SetGamingProfile`

`GetGamingProfile(0)` returns one setting per output byte. In `SetGamingProfile`, byte 0 is a
mask of which bytes to write: bit n selects byte n + 2. The rest of the word is left alone.

| Setting | Byte | Set input |
|---|---|---|
| Win key (1 = Windows and Menu keys work) | 3 | `0x02 \| on << 24` |
| LCD overdrive | 6 | `0x10 \| on << 48` |

LCD overdrive lives here, *not* in misc 6. Misc 6 is the boot animation. Several tools mix them up.

## Keyboard

| Setting | Call |
|---|---|
| Zone color | `SetGamingRgbKb(zone bit \| R << 8 \| G << 16 \| B << 24)`; `GetGamingRgbKb(zone bit)` reads it back. Zone bits 1, 2, 4, 8; the PT515-51 has zones 1–3. |
| Effect | `SetGamingKBBacklight(uint8[16])`: `[mode, speed, brightness, pattern, direction, R, G, B]`, repeated in bytes 8–15 |
| Backlight off when idle | `APGeAction`: get `0x88401` returns brightness in byte 4 and idle seconds in byte 5 (0 = never); set `0x88402 \| brightness << 32 \| seconds << 40` |

Effect details:
- **Modes:** 0 Static, 1 Breathing, 2 Neon, 3 Wave, 4 Shifting, 5 Zoom.
- **Fields:** speed 1–9, brightness 0–100, direction 1 (right to left) or 2 (left to right).
- **Pattern** is `0x08` for Wave and Shifting.
- **Color** is used by Breathing, Neon and Zoom; **direction** by Wave and Zoom.
- `GetGamingKBBacklight` is rejected, so the effect can't be read back.

For the backlight timeout, `0x84` in byte 1 is a keyboard selector. PredatorSense reads it from a
registry value set on Acer's factory image (`BK_Hotkey_Number`). It is the only value the
PT515-51 accepts. Any number of seconds is accepted; 0, 30, 60 and 120 were verified.

## System

| Setting | Call |
|---|---|
| USB charging when off | `APGeAction`: get `0x204` (`0x4` and `0x104` answer the same) returns state in byte 1 (`0x0F` on, `0x1F` off) and the battery floor % in byte 2; set `0x04 \| state << 8 \| floor << 16`, floors 10 / 20 / 30, off sent with floor 10 |
| Battery temperature | `BatteryControl.GetBattInfoInterface(uBatteryInfoIndex = 8, uBatteryNo = 1)`: tenths of a kelvin |
| Battery health (80 % limit) | `BatteryControl.GetBatteryHealthControlStatus(1, 1, [0, 0])` returns `uFunctionList = 0` and `uReturn = 1` on the PT515-51: **not supported** |

## Buttons

| Button | What arrives |
|---|---|
| PredatorSense | Keyboard scan code `E0 75`, virtual key `0xFF` |
| Turbo | Nothing. The firmware toggles misc 1 and misc 5 itself, and Predator Lite polls misc 1 once a second. |

## Not supported on the PT515-51

All of these were probed read-only:
- battery health / calibration (`BatteryControl`);
- `GetGamingFanTable` (status 1);
- `GetGamingLEDBehavior` / `GetGamingLEDColor` (status 1; no extra LEDs);
- misc indexes 7–16;
- Dust Defender (`APGeAction 0x0307` returns `0xE1`);
- sensor ids 4, 7, 8 and 11.

## Verification status

**Read-only, on hardware:**
- capability detection, all sensors, fan modes, Turbo LED state, misc settings, zone colors;
- the WMI marshalling (`uint32`, `uint64` as string, `uint8[]`).

**With writes, on hardware** (`examples/hardware_write_test.rs` and ad-hoc scripts, state
restored after each):
- Turbo on and off: LED, turbo flag, fans at Max and back, GPU level Turbo and back.
- Fans Max and Auto: RPM rose from 3366 / 5342 to 4818 / 6301 and fell back.
- Zone color change and restore.
- Win key: only byte 3 of the profile word changed.
- Keyboard backlight timeout: 30, 0, 60 and 120 s, brightness kept.
- USB charging when off: 20 %, off, 10 % and 30 %.
- Custom fan % against the duty sensors, in 10 % steps.

**Buttons:** the PredatorSense button arrives as `E0 75`. The Turbo button flips the firmware flag
and GPU level with no event.

**Boot animation:** switched on from the app and confirmed at power-on; it reads back from misc 6.

**From the original's code only:**
- the LCD overdrive write. The read (byte 6 = 1 with overdrive on) was checked, the write was not.
- the keyboard effect payload, though each effect was checked by eye on the keyboard. Neon and
  Shifting needed the original's 4-zone builder: another layout left the keyboard dark.
