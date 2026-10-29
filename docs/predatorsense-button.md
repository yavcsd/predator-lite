# PredatorSense button

The PredatorSense button above the keyboard opened Acer's app. Predator Lite can use it in two ways:

| | Setup | What the button does |
|---|---|---|
| **App running** (in the tray or open) | none | Opens the window, or hides it to the tray if it is already in front |
| **App closed** | one-time script + restart | Starts Predator Lite with its window open |

## While Predator Lite is running

Nothing to set up. Predator Lite listens to the button directly, through raw keyboard input,
whichever window is in front. Press it once to bring the window up, and again to hide it.

If you [start Predator Lite at logon](installation.md#start-at-logon), this is all you need: it is
always running in the tray.

## Make the button start Predator Lite when it is closed

Windows doesn't know what to do with the PredatorSense button by itself. Its scan code (`E0 75`)
isn't a standard key. The setup script fixes that:

1. It turns the button into a standard key, **Launch App 1** (`E0 6B`), with a keyboard remap.
   Any remaps you already have are kept.
2. It makes Launch App 1 open a shortcut. This is for your user only; Windows' default for
   Launch App 1 ("This PC") stays in place for other users.
3. The shortcut starts a scheduled task, **Predator Lite (button)**, that runs
   `predator-lite.exe --toggle` with administrator rights, so pressing the button never shows a
   UAC prompt. It runs through a hidden console, so nothing flashes on screen.
4. `--toggle` starts the app with its window open. If Predator Lite is already running, it just
   shows or hides the window instead of starting a second copy.

### Install

In an **administrator** PowerShell in the Predator Lite folder (see
[Running the setup scripts](installation.md#running-the-setup-scripts)):

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\install-predatorsense-button.ps1
```

You should see:

```
task:      'Predator Lite (button)' -> C:\Tools\Predator Lite\predator-lite.exe --toggle
shortcut:  C:\Users\<you>\AppData\Local\PredatorLite\PredatorSense button.lnk
app key:   Launch App 1 -> shortcut
remap:     E0 75 -> E0 6B (1 entry in Scancode Map)

Done. Restart Windows to activate the key remap.
```

**Restart Windows.** Keyboard remaps are only read at startup. Then close Predator Lite (tray
icon > Exit) and press the PredatorSense button: the window should appear within a second.

If `predator-lite.exe` isn't next to the `tools` folder, pass its path with
`-Exe "D:\Apps\predator-lite.exe"`. Run the script again with the new path if you move the
program later.

### Limits

- **An administrator window in front.** While an elevated window has focus (Task Manager, an
  administrator terminal, …), Windows doesn't pass the key to Explorer, so a *closed* Predator
  Lite can't be started. A *running* one still reacts. Starting the app at logon avoids this
  entirely.
- **Other models.** The scan code `E0 75` was measured on a Predator Triton 500 (PT515-51). If
  your button sends something else, the remap does nothing. Developers can check with the
  `capture_hotkeys` tool, see the [developer guide](developer-guide.md#research-tools).

### Uninstall

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\uninstall-predatorsense-button.ps1
```

This removes the remap (other remaps are kept), the shortcut and the task. It sets Launch App 1
back to the Windows default if it still points to the Predator Lite shortcut. Restart Windows to
give the button its original scan code back.

### Troubleshooting

- **Nothing happens when the app is closed.**
  - Did you restart after installing? Check that the remap is there. In PowerShell, run
    ```powershell
    [BitConverter]::ToString((Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Keyboard Layout').'Scancode Map')
    ```
    The output should contain `6B-E0-75-E0`.
  - Check that the task works: run `schtasks /run /tn "Predator Lite (button)"`. The app should
    start.
- **"This PC" opens instead.** The per-user Launch App 1 setting is missing. Run the install
  script again, from the account you use.
- **The window opens and closes at once.** This shouldn't happen: a running instance ignores the
  duplicate signal it gets from one press. If it does, turn on the
  [debug log](user-guide.md#debug-log), press the button, and include the log in an issue.
