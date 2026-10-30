# Changelog

## 0.1.1 (2026-10-30)

- **Fixed:** the GPU mode box showed *Discrete only* at start while the laptop ran in Hybrid. The
  firmware reports Discrete after every start on the PT515-51, so the box now shows the mode
  Windows is running in, or the one chosen since Windows started. A switch is always written to
  the firmware, even when it already reports that mode.
- `--status` reports the running GPU mode next to the firmware value.
- **Docs:** on the PT515-51 a full shutdown turns the firmware's Win key lock back on (a
  restart doesn't). Predator Lite turns it off again when it starts and after sleep; the user
  guide now says so and points to starting it at logon.

## 0.1.0 (2026-10-29, first public release)

- **Monitoring:**
  - CPU, GPU and system temperatures.
  - RPM and real duty cycle of all three fans.
- **Fans:**
  - Auto, Max or Custom per group, with Custom calibrated to real duty.
  - CoolBoost.
- **Modes:**
  - Turbo (also from the physical Turbo button).
  - Balanced and Eco, with software fan curves.
  - Eco also turns Turbo Boost off through its own power plan and caps the GPU clock via
    `nvidia-smi`.
- **GPU and display:**
  - GPU overclock levels.
  - GPU mode (Hybrid / Discrete only) with a restart prompt.
  - LCD overdrive.
  - Battery rule for overclocking.
- **Keyboard:**
  - Zone RGB (up to 4 zones), static and the firmware effects.
  - Win key lock.
  - Backlight off when idle.
- **System:**
  - Boot animation and sound.
  - USB charging when off.
- **Buttons and tray:**
  - PredatorSense button: toggles the window, and can start the app with the setup script.
  - Tray icon and menu.
- **Settings:**
  - Saved in `settings.ini` and restored at start and after sleep.
  - Firmware-owned settings are read, never overwritten.
  - The keyboard lighting is left as it is until you change it in the app (the firmware can't
    report its effect).
- **Tools:**
  - Setup scripts for start at logon and the PredatorSense button.
  - Read-only `--status` report.
