# Developer guide

- [Code layout](#code-layout)
- [How a change reaches the firmware](#how-a-change-reaches-the-firmware)
- [Building and testing](#building-and-testing)
- [Research tools](#research-tools)
- [Adding support for another laptop](#adding-support-for-another-laptop)
- [Conventions](#conventions)
- [Making a release](#making-a-release)

## Code layout

Predator Lite is a library crate (`src/lib.rs`) plus a thin binary (`src/main.rs`). Keeping
everything in the library means the unit tests build without the administrator manifest, which
is embedded only into the binary by `build.rs`.

| File | Role |
|---|---|
| [`src/protocol.rs`](../src/protocol.rs) | Bit-level encoding of the firmware calls: input layouts, output decoding, the fan duty calibration. Pure functions, no I/O. See [firmware-interface.md](firmware-interface.md). |
| [`src/controller.rs`](../src/controller.rs) | `Controller`: turns `Settings` into firmware calls. Capability detection, Turbo / Balanced / Eco, battery rule, Turbo button, fan curve ticks, rollback on failure. Contains the `mock` firmware and most unit tests. |
| [`src/settings.rs`](../src/settings.rs) | `Settings` (what the user chose) and the `settings.ini` format: lenient parsing, old keys, migrations. |
| [`src/fan_curve.rs`](../src/fan_curve.rs) | `FanCurve` (validated points) and `FanTrack` (smoothing, hysteresis, gradual ramp-down). |
| [`src/eco_limits.rs`](../src/eco_limits.rs) | Eco's Windows-side limits: the "Predator Lite Eco" power plan and the `nvidia-smi` clock cap. |
| [`src/wmi.rs`](../src/wmi.rs) | COM/WMI plumbing: connecting to `ROOT\WMI`, calling methods with the right argument types, event subscriptions. |
| [`src/gui.rs`](../src/gui.rs) | The window and tray icon, in plain Win32 (standard controls, no framework), timers, hardware-button input, single-instance handling. |
| [`src/status_report.rs`](../src/status_report.rs) | `--status`: a read-only report built from `Get*` calls only. |
| [`src/system.rs`](../src/system.rs) | Small Windows helpers: starting System32 programs without a console window, reading the power status. |
| [`examples/`](../examples) | Research and test tools, not shipped. See [below](#research-tools). The capture tools share `examples/common/`. |
| [`tools/`](../tools) | PowerShell setup scripts for start at logon and the PredatorSense button. |

## How a change reaches the firmware

1. A control in `gui.rs` calls a `Controller` setter, e.g. `set_fan_mode`.
2. The setter checks the capability and the value range. It then changes `Settings` inside
   `update`, which calls `apply(parts)` for the affected parts only (`part::FANS`, …).
3. `apply` derives what the firmware should hold. For example, Turbo forces the fans to Max and
   battery power forces the GPU to Normal. It reads each value first and writes only what
   differs.
4. If the firmware rejects a write, `update` restores the previous `Settings` and applies those
   again.
5. On success, `gui.rs` saves `Settings` to `settings.ini`.

At start and after sleep, `Controller::restore` re-applies the saved settings (`part::RESTORE`).
Firmware-owned settings (`part::FIRMWARE_OWNED`: LCD overdrive, boot animation, USB charging)
are read with `sync_firmware_owned` instead. The keyboard lighting is skipped until the user has
set it in the app (`Settings::restore_kb_lighting`), because its effect can't be read back.

The `Backend` trait is the only way the controller talks to hardware. The real one is
`wmi::Wmi`; the tests use `controller::mock::Mock`. It answers from replies captured on a
PT515-51 and remembers writes the way the firmware does.

## Building and testing

Requirements: Windows 10/11 and Rust stable with the MSVC toolchain (`rustup default
stable-x86_64-pc-windows-msvc`).

```
cargo fmt                     # rustfmt.toml: 120 columns
cargo test                    # unit tests: no admin rights, no hardware access
cargo clippy --all-targets    # perf, style, complexity, pedantic: must be warning-free
cargo build --release         # target\release\predator-lite.exe
cargo build --release --examples
```

- The release profile optimises for size: `opt-level = "s"`, LTO, `panic = "abort"`, stripped
  symbols.
- `.cargo/config.toml` links the C runtime statically, so the exe runs without the Visual C++
  redistributable.
- **The release exe can't be rebuilt while Predator Lite is running.** Windows locks it. Exit it
  from the tray first.

## Research tools

The programs in `examples/` were used to decode the firmware. They are useful when bringing up
another model. All of them need an elevated prompt (`target\release\examples\<name>.exe
<report-file>`), and they write a text report.

| Tool | Writes to firmware? | What it does |
|---|---|---|
| `capture_button_inputs` | no | 30 s capture of every source a hardware button could use: all `ROOT\WMI` events, non-typing keys, raw HID input |
| `capture_hotkeys` | no | 30 s capture of `APGeEvent` events and non-typing keys. Letters, digits and punctuation are never logged. |
| `watch_turbo_state` | no | Polls the Turbo-related firmware state every 200 ms for 40 s and logs each change |
| `system_integration_test` | no firmware writes; changes the power plan, restored at the end | Live WMI connection and every read the app makes, the `APGeEvent` subscription, Eco's power plan (created, activated, Turbo Boost off, user plan restored, old plan name renamed, lost record) and the `nvidia-smi` clock lock, all checked with `powercfg`. Close Predator Lite first. |
| `hardware_write_test` | **yes**, restored at the end | Drives the real `Controller`: Turbo on and off, fans Max and Auto, a zone color. Every write is checked with a read-back. Close Predator Lite first. |
| `turbo_button_events` | **yes**, restored | Arms the Turbo key the way PredatorSense does, then listens for events while you press it |
| `turbo_rearm_probe` | **yes** | Guided experiment: one kind of write after each Turbo-button press, watching whether the firmware re-engages Turbo |

`predator-lite.exe --status report.txt` is the first thing to run on a new machine. It only
reads.

## Adding support for another laptop

1. **Collect a status report.** Run `--status` and keep the report. It shows which capabilities
   were detected and the raw answers.
2. **Check what's greyed out.** Capability detection (`Capabilities::detect`) is read-only and
   per feature, so a model that lacks something simply has it greyed out. Look for features that
   are detected but behave differently.
3. **Recalibrate the fans if needed.** The fan duty table in `protocol.rs` was measured on a
   PT515-51. Compare the duty readout in the window with the percentage you set. If they differ a
   lot, the table may need a per-model variant.
4. **Check the buttons.** Use `capture_hotkeys` to find what the PredatorSense and Turbo buttons
   send.
5. **New firmware calls.** Document their source: which PredatorSense function, or which
   community project. Test each write with a read-back on real hardware, restoring the original
   value. Add the encoding to `protocol.rs` with a unit test, the controller logic with mock
   tests, and a row to [firmware-interface.md](firmware-interface.md).
6. **Open a pull request** with the model name, BIOS version and what you verified.

Things that need care on any model:
- **Misc writes** that repeat the current value can act like a Turbo button press. Always use
  the read-compare-write pattern (`Controller::set_misc` and friends).
- **Firmware-owned settings** must be read at start, not restored.
- **Never write raw EC registers.** Use the BIOS's own WMI methods.

## Conventions

- `protocol.rs` holds encodings only, each with a doc comment saying where it came from and how
  it was verified, plus a unit test with captured values.
- Every firmware write path in `controller.rs` gets a mock test, including the failure and
  rollback case.
- Settings keys are stable. When one changes, `settings::parse` keeps reading the old name (see
  the "old" tests in `settings.rs`).
- Comments explain *why*, record hardware findings, and avoid restating the code.
- `cargo fmt --check`, `cargo test` and `cargo clippy --all-targets` must pass with no warnings.
  CI runs all three on every push.
- Clippy runs the `perf`, `style`, `complexity` and `pedantic` groups, set in the `[lints]`
  table of `Cargo.toml`, so plain `cargo clippy` uses them. No lint is allowed crate-wide, so
  `cargo clippy --all-targets -- -W clippy::perf -W clippy::pedantic -W clippy::style
  -W clippy::complexity` gives the same, empty, result. CI runs that command with `-D warnings`.
  - Conversions between integer types use `From` / `TryFrom`, `to_le_bytes` (see
    `protocol::byte`) or `cast_signed` / `cast_unsigned`.
  - The few intended lossy casts live in small helpers with a local `#[allow(..., reason = ...)]`:
    `system::size_u32`, `protocol::round_to_u8`, and `loword`, `hiword`, `row_id`, `fan_row`
    and `scale_px` in `gui.rs`.
  - Every public function that returns a `Result` has an `# Errors` section.
  - `clippy.toml` lists product names that may appear in doc comments without backticks.

## Making a release

1. Bump `version` in `Cargo.toml` and the `assemblyIdentity` version in `app.manifest` (four
   parts, e.g. `0.2.0.0`), and add the changes to `CHANGELOG.md`.
2. Commit, then tag and push: `git tag v<version>` and `git push origin v<version>`.
3. The [release workflow](../.github/workflows/release.yml) checks that the tag matches
   `Cargo.toml`, runs the tests, builds `predator-lite.zip` and attaches it to a GitHub release.
   The zip holds `predator-lite.exe`, `README.md` and `LICENSE` at the top level, and the
   `tools` folder next to them. The setup scripts find the exe one level up from `tools`.
