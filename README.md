# IluTrackball

Windows tools that remove the stepped cursor motion of the Logitech MX Ergo trackball.

- `ilumx-probe` records the trackball's input and finds the cause of the steps. It only observes and leaves the cursor and other mice alone.
- `ilumx-replay` plays recorded logs through filter modes and draws charts to compare them.
- `IluTrackball` filters mouse motion live. It has a small settings window and does not require a driver.

The ready-made program is in [Releases](https://github.com/ilumetric/IluTrackball/releases/latest). It is a single file, `IluTrackball.exe`: put it in any folder and run it. Its settings appear next to it. The file is not signed, so Windows may show a SmartScreen warning the first time you run it.

The usual order is to record logs, read the report, pick parameters in replay, and turn on the filter.

The window of `IluTrackball` is available in English and Russian. `ilumx-probe` and `ilumx-replay` print their prompts and reports in Russian.

## Building

You need Rust and the Visual Studio Build Tools.

```bash
cargo build --release
```

The programs appear in `target\release\`.

## Recording logs

Run this from the root of the project. Logs go to `logs\<date_time>\`.

```bash
./target/release/ilumx-probe.exe record
```

The program offers a device. The MX Ergo is selected by default, so pressing Enter is enough. Four 10 second scenarios follow, and each attempt can be recorded again or skipped. At the end the report appears in the console and in `report.md`.

Only the packets of the selected device are recorded.

## Commands

| Command | What it does |
|---|---|
| `ilumx-probe list` | Shows all mice and their identifiers |
| `ilumx-probe record` | Records the scenarios and builds a report |
| `ilumx-probe analyze <folder>` | Builds the report again from existing logs |

Useful options:

| Option | Commands | Meaning |
|---|---|---|
| `--device N` | `record` | Device number from `list` |
| `--seconds N` | `record` | Length of a scenario, 10 by default |
| `--out <folder>` | `record` | Where to write the logs |
| `--no-prompt` | `record` | Record all scenarios in a row without asking |
| `--pause-ms N` | `record`, `analyze` | Shortest stick-slip pause, 40 ms by default |
| `--jump N` | `record`, `analyze` | Smallest stick-slip jump, 4 counts by default |

## Reading the report

| Cause | What confirms it |
|---|---|
| Double frames | Packets twice the size of their neighbours, in a steady rhythm, while the intervals stay perfect |
| Uneven polling | Gaps, bunched packets and pauses where the ball is turning fast |
| Quantization | In slow stretches almost all steps are 1 count, and 1 count makes more than 1 pixel |
| Stick-slip | In slow motion a pause and then a jump in the same direction, with no such pauses in fast motion |

A score of 1.00 or more means a clear problem. The thresholds were chosen on synthetic data so far and will be refined on real logs.

Windows settings are read when the report is built, so `analyze` shows the current values and not those at the time of recording.

## Choosing a mode

```bash
./target/release/ilumx-replay.exe logs/<date_time>
```

The logs are played through a set of modes. The result goes to the `replay` folder next to the logs: a table in the console, charts in `report.html` and the processed logs as CSV.

Your own variants are given with `--spec`, which can be repeated:

```bash
./target/release/ilumx-replay.exe logs/<date_time> --spec passthrough --spec debeat:release_ms=30+ema:tau_ms=4
```

The parts of a mode are joined with `+` and run in order:

| Part | What it does | Parameters |
|---|---|---|
| `passthrough` | Nothing, the reference for delay | |
| `debeat` | Trims double steps and spreads the excess. Has to come first | `ratio`, `window_ratio`, `max_ratio`, `release_ms`, `period_ms` |
| `spread` | Spreads each packet over a window of time | `ms` |
| `ema` | Plain smoothing | `tau_ms` |
| `one_euro` | Heavy smoothing of slow motion, light smoothing of fast motion | `min_cutoff`, `beta`, `d_cutoff` |
| `precision` | A constant multiplier | `factor` |
| `accel` | An acceleration curve | `type=linear`: `base`, `slope`, `cap`. `type=power`: `base`, `scale`, `exponent`, `cap`. `type=table`: `points=speed/gain;...` |
| `tail` | Longest tail after the ball stops | `ms` |

The speed for `accel` is measured in counts per millisecond.

The columns of the table:

| Column | Meaning |
|---|---|
| Лаг (lag) | Average delay behind the original motion, ms |
| Неровность (roughness) | How much steps differ from their neighbours |
| Двойные шаги (double steps) | How many steps twice the size of their neighbours are left |
| Неровность по кадрам (roughness by frame) | The same for steps between monitor frames. The rate is set with `--refresh`, 60 Hz by default |
| Отставание (trailing) | The largest gap from the original position, px |

## The filter

```bash
./target/release/IluTrackball.exe
```

The mouse, preset and processing are on the left, the acceleration curve and the test area on the right. At the bottom are the RU/EN language switch and the settings button.

### Living in the tray

The close button hides the window and the program keeps working. A red ball next to the clock means the filter is working, a grey one means it is not acting right now. Hover over it to see why.

- A click on the icon opens the window.
- The right button opens a menu with the current state, Open, Pause and Quit.
- The program only quits through this menu or the emergency shortcut.
- Starting a second copy just shows the window of the first.
- If Explorer restarts, the icon comes back by itself.

### Updates

Twice a day the program asks GitHub whether a new version is out. The Check for updates switch in the settings turns this off, and the Check now button always works. When a version is out, a green dot appears on the settings button and the icon menu gets an Update to item. Nothing is downloaded until you press Update and restart in the settings.

What happens during an update:

1. The new file is downloaded next to the program and compared with the checksum from the release.
2. The new version is started as a test. If the file cannot start, everything stops and the current version stays as it was.
3. The files change places, the program lets go of the mouse and starts the new version. The settings stay where they are and do not change.
4. The old file is kept as `IluTrackball.old.exe` for the first minute. If the new version fails in that time, the old one comes back and starts by itself.

The checksum shows that the file arrived whole. It sits in the same release, so it does not protect against a replaced release: for that the program relies on HTTPS and on the GitHub account. Files are only accepted from the releases of this repository.

If the program's folder cannot be written to, as with `Program Files`, updating needs administrator rights or moving the program to another folder.

### Mice

The drop-down list holds all mice of the computer. Each has its own switch and its own preset.

- Choose a mouse in the list and its settings appear in the window.
- The switch next to the list turns the filter on for that mouse. Mice that are switched off work directly, as if the program were not there.
- The green dot lights up while a mouse moves, which makes it easy to tell them apart.
- The test area shows the selected mouse even when it is switched off, so a preset can be judged without touching the cursor.
- With all mice switched off, neither a hook nor a driver stays in their path.

Windows cannot tell apart the mice on one receiver, so they share one line with common settings.

### Presets

Standard presets are built into the program and never change. When you move a slider on a standard preset, your own copy appears with "(mine)" added, and the original stays as it was.

| Button | What it does |
|---|---|
| Plus | Saves the current settings as a new preset |
| Pencil | Renames your preset |
| Trash | Deletes your preset. Mice using it go back to Balanced |
| Down arrow | Loads a preset from a file |
| Up arrow | Saves the preset to a `.ilutrack` file, which can be passed to someone else |

### Finding your settings

The Find my settings button starts a test of about a minute. Dots appear one after another: move the cursor to the dot and click. The test runs on neutral settings, so the result does not depend on the preset that was on before it.

| Dots | What they show | What follows from it |
|---|---|---|
| Ordinary, 12 of them | The rhythm of the link and the double steps | The recording is played through filter chains and the best balance of smoothness and delay is chosen |
| Small and close together, 10 of them | The smallest movement of the hand and the speed of careful work | The smallest nudge moves the cursor by about one interface pixel, and the curve stays flat at these speeds |
| Far apart, 6 of them | How far the ball travels in a flick and how fast | The curve rises so that a typical flick travels the wanted distance |

The Windows pointer speed and the display scale are taken into account. On the result page the settings are already at work: try them right away, choose Quicker, Balanced or Smoother, and set the flick reach. Saving gives an ordinary preset called Tuned, which can be adjusted like any other. Cancelling puts everything back.

This is a place to start from, not a verdict: tastes in smoothness and speed differ.

### How it works

The settings offer two ways:

| Way | What it does | What it needs |
|---|---|---|
| No driver | Filters the cursor with a mouse hook and `SendInput` | Nothing |
| Interception driver | Filters at driver level, games included | The driver and `interception.dll` |

Start with No driver. The program installs nothing, and everything works as before after it quits.

Limits of No driver:

- The filter does not reach games using Raw Input: they read the mouse directly, and the motion would reach them twice. So while a game holds the cursor (it is hidden and the window fills the screen, or the cursor is locked in), the filter steps aside. Leave games alone in the settings turns this off.
- While a window with administrator rights is active, such as Task Manager, Windows does not let an ordinary program move the cursor and silently throws the motion away. The filter steps aside for that time and the mouse works as usual. The same happens in the panel of hidden tray icons. Run as administrator in the settings removes both limits, see below.
- When you switch from one mouse to another, one packet may miss the filter or get lost. Windows does not tell a hook which mouse an event came from, so that is found out through Raw Input a moment later.

Turn off "Enhance pointer precision" in the Windows mouse settings. Its acceleration depends on the packet size, so with smoothing the cursor gets slower.

### Controls

| Shortcut | Action |
|---|---|
| `Ctrl+Alt+F11` | Next preset for the mouse that moved last |
| `Ctrl+Alt+Pause` | Emergency exit: let go of the mouse at once and quit |

Starting with Windows is turned on in the settings and is off by default. The program then starts in the tray and goes to work at once.

### Run as administrator

The switch in the settings is off by default. Windows asks for permission once and creates a scheduled task named `IluTrackball`, which then starts the program with full rights and no prompt, whichever way you start it. Start with Windows goes through the same task instead of the Run key. The program restarts when you turn the switch on. When you turn it off, the task is removed, and the rights last until the program quits.

Keep the program in a folder other programs do not write to: whoever replaces `IluTrackball.exe` gets the same rights at its next start. Updates still work the same way, and the new version keeps the rights.

The switch needs a Windows account with administrator rights. Turn it off before you delete the program, or the task stays behind.

Settings are kept in `ilutrackball.toml` next to the program. Changes to the file are picked up on the fly. The old settings format is converted by itself on the first run.

The `--demo <log.csv>` option plays a log in the test area in a loop, so there is no need to turn the ball. The recording does not move the real cursor.

### Protection against hangs

All of this works with the window hidden too: capturing does not depend on the interface.

- Only one copy of the program runs: a second one just shows the window of the first.
- Settings are written through a temporary file, so a failure cannot leave them damaged.
- The filter keeps working after the computer sleeps, and the watchdog allows for that.
- The program compares what the hook sees with what comes through Raw Input. If Windows removed the hook, it is installed again. If packets stop coming, the hook lets go of the mouse and the registration is renewed.
- If a mouse cannot be identified, for example right after a Bluetooth reconnect, the program asks again and does not leave it unidentified for good.
- The log `ilutrackball.log` next to the program shows every change of state and the program in front. If the mouse behaves strangely, that is the place to look.
- The emergency exit runs on a separate thread and does not depend on the rest of the program.
- If the input thread stays silent for more than 0.3 s, the hook lets go of the mouse. After 5 s of silence (1.5 s with the driver) the program starts a fresh copy in the tray and quits. Windows then removes the hook and closes the driver by itself.
- The same happens when the program crashes. A copy that fails within a minute of such a restart starts no new one: a restart loop on the mouse path is worse than a plain mouse.
- Service threads start again after a failure.
- If the filter fails, motion carries on unprocessed and the window says so.
- Clicks and the wheel do not go through the filter. With the driver they stay inside it, and the hook only intercepts motion.
- The smoothing tail is cut off 200 ms after the last packet. The `tail` part sets this value.

### The Interception driver

**Try No driver first.** Install the driver only if that is not enough. There is deliberately no install button in the program: a failed installation can leave the computer without a mouse and keyboard.

**Important: the driver is old, and Memory integrity (under Core isolation in Device security) may block it from loading.** Interception attaches as a class filter to all mice and keyboards. If Windows refuses to load it, all input may be gone after a reboot. Compatibility of the driver with Memory integrity has not been tested.

Create a restore point before installing. Whether to turn Memory integrity off is your decision: doing so weakens the protection of the system.

Installation:

1. Download `Interception.zip` from its [releases page](https://github.com/oblitum/Interception/releases) and unpack it.
2. Open a command prompt as administrator in the `command line installer` folder.
3. Run `install-interception.exe /install`.
4. Reboot.
5. Copy `library\x64\interception.dll` next to `IluTrackball.exe`.
6. Start the filter and choose Interception driver.

Removal: run `install-interception.exe /uninstall` as administrator and reboot.

If input is gone after a reboot, start the Windows Recovery Environment by interrupting the boot with the power button three times in a row. Choose System Restore there and roll back to the restore point you created.

What to know about the driver:

- Anti-cheat systems in games may object to it.
- In this mode all key presses pass through the program, so that the emergency exit works. They are sent on at once and are not stored anywhere.
- With the driver only the mice you switch on are captured: other mice do not pass through the program at all.

## Log format

CSV with the header `t_qpc_us,dx,dy,buttons,wheel`. Time is in microseconds from the start of the scenario. `buttons` is a mask of held buttons: left 1, right 2, middle 4, X1 8, X2 16.

## Tests

```bash
cargo test --workspace
```

The call to the real GitHub is a separate test, because it needs the internet:

```bash
cargo test -p ilumx-filter -- --ignored the_real_github_answers
```

A debug build can be pointed at a local server with the `ILUTRACKBALL_TEST_SERVER` variable, to go through a whole update without publishing anything. A release build has no such option.

## Publishing a release

GitHub Actions builds releases: sending a tag is enough. The version in the tag has to match the version in `Cargo.toml`, or the build stops.

1. Change `version` in `Cargo.toml`, for example to `0.2.0`.
2. Build the project so that `Cargo.lock` is updated, and commit the changes.
3. Create a tag and send it along with the branch:

```bash
git tag v0.2.0
```

```bash
git push origin main v0.2.0
```

The build runs the tests, builds `IluTrackball.exe`, works out the checksum and publishes a release with notes taken from the commits. Its progress is on the Actions tab. After that the program offers the update to everyone who uses it.
