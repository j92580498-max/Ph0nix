# Native applications

ph0nix uses native application windows on Linux and Windows,
and native Android activities. The K750i apps launch the same DB2010 QEMU board
through a Rust host; Python and a browser are not required at runtime.

The interface is based on [J2ME-Loader](https://github.com/nikita36078/J2ME-Loader)
revision `afffdde4e677b3d5ef516e713732abc488f19fc0`: firmware library, search,
grid/list views, sorting, import action, entry menus, and device/settings pages.
Its Apache-2.0 attribution and license are included in the application. Firmware
execution uses the device backends, rather than J2ME-Loader's MIDlet runtime.

| Platform | Application | Current validation |
| --- | --- | --- |
| Linux | Rust egui/eframe window and native QEMU child | ARM64 build and native transport; desktop window checks recorded with the test artifacts |
| Windows | Same native launcher and QMP host; MSYS2 UCRT64 build and DLL packaging | Build workflow provided; Windows execution has not been tested on this Linux ARM64 host |
| Android | Kotlin/Material native UI, Rust JNI worker, packaged Android QEMU PIE | ARM64 debug APK; device installation/runtime testing remains necessary |

## Testing the APK

The release artifact is `ph0nix-1.0-android-arm64.apk` on the
[Ph0nix 1.0 release page](https://github.com/j92580498-max/Ph0nix/releases/tag/v1.0).
It targets Android 7.0/API 24 and later on **arm64-v8a** devices. It is signed
with the project's existing development certificate to preserve upgrades from
earlier test APKs. The release variant is not debuggable. Other Android ABIs
are not packaged.

1. Install the APK and open **ph0nix**.
2. In **Library**, tap **+**, choose **Sony Ericsson K750i**, and select both
   `K750_R1DB001_MAIN_EU_1_CL_RED49.mbn` and
   `K750_R1DB001_FS_EMEA_1_RED49.fbn` together.
3. Tap the imported entry. The firmware draws its original startup and setup
   screens. First boot can take several minutes. Choose a language with the
   keypad, and complete or skip the native setup wizard.
4. From standby, press **OK**, then **#** to open Settings. **Left/Right** switch
   General, Sounds & alerts, Display, Calls, and Connectivity. **OK/Left soft**
   open or select an item; **Back** and **C** are separate K750i contacts.

Full MAIN/FS images are supplied by the user. The supplied
[firmware.center K750 archive](https://firmware.center/firmware/Sony-Ericsson/K750/)
contains several revisions; this profile accepts only the pinned R1DB001 CID49
pair. Import validates complete containers and their sparse payload records.

**Settings** controls sound, the FPS overlay, fullscreen, keeping the display awake,
and whether K750i resumes its previous session. Leaving the phone saves its
internal storage in a background worker. Key contacts release on
focus loss or backgrounding. Android storage access uses document selection
and private copies. The Internet permission is used for loopback QMP; no web
application is started.

Desktop Settings keeps a draft until **Save**. **Cancel** or switching tabs
discards the draft and restores any unsaved fullscreen preview.

Disabling resume boots the firmware from reset with the saved internal storage
and settings. Enabling resume continues the saved session when available. The
K750i entry menu in Android can export or import `phone-memory.k750mem`; an
import always starts a fresh boot with the restored storage. Imported moves wait
for a durable save before deleting the host source. A failed save or changed
source keeps the original. See [storage](storage.md) for the save format.

## Developer mode and transfers

Open **Developer** from the running phone's toolbar/menu to see diagnostics.
All keypad contacts go to the firmware; the native service menu opens with
**Right, Star, Left, Left, Star, Left, Star** (`→ * ← ← * ← *`) from standby.
The phone continues running while the developer window shows metrics, bounded logs and
errors. Reports can be copied on Android or copied/saved on desktop.

Metrics distinguish actual UI presentations, firmware LCD updates, polling
rate and guest clock speed. CPU and resident memory are measured separately
for the app and the QEMU process; 100% CPU represents one occupied core.
Stage timings include QMP, LCD decoding, rendering and PCM reception. Sampled
engine timer costs show scheduling and service calls without changing guest
checkpoint state. Unavailable platform counters are left unset.

Open **Phone files** from the running phone's toolbar/menu. The browser uses
the firmware's real UART0 PC Suite OBEX service to list folders and copy or
move files. Host moves delete the original after successful verification;
Android uses document providers and keeps the source when the provider cannot
verify or delete it. Files are streamed with a 32 MiB transfer limit. See
[storage.md](storage.md) for reproducible validation and format details.

**Bluetooth files** uses the host's actual Bluetooth adapter and the standard
RFCOMM Object Push service. Pair devices in the operating system first.
Android requests Bluetooth permissions when the feature is selected. Received
files stay in a private inbox until imported into the phone; sending a phone
file first exports it through the firmware service. Some Android systems route
Object Push to their built-in receiver; **Import received file…** opens the
document picker for these downloads. Linux receiving requires BlueZ,
`libbluetooth.so.3` and its local SDP compatibility socket. These Bluetooth
protocols have automated socket tests; physical RF transfers remain untested.
This bridge does not emulate the firmware's UART3 Bluetooth controller.

**Card images** creates and browses raw FAT16 images from 16 to 1024 MiB. Android
can import/export image files with the document picker. The image filesystem
supports folders, copy/move, import/export and deletion without loading the
whole disk into memory. **Mounting an image in K750i firmware remains unfinished**:
the phone uses Memory Stick Duo and its card controller is not implemented.

## Linux

Install Rust, Python 3, C/C++ tools, CMake, Ninja, pkg-config, GLib development
files, GTK 3 development files, libclang, and zlib. On Debian/Ubuntu:

```sh
sudo apt-get install build-essential cmake ninja-build pkg-config \
  libglib2.0-dev libgtk-3-dev libclang-dev libxkbcommon-dev zlib1g-dev libasound2-dev
git submodule update --init qemu
python3 frontends/build_desktop.py --jobs 2
```

The portable directory is `dist/ph0nix-linux-<architecture>/`.
Run `ph0nix` inside it. Firmware import accepts the same MAIN/FS
pair. GTK 3 and GLib runtime libraries must be installed. Builds use the host's
libc; a binary built on a newer distribution may require rebuilding on an older
distribution.

For development with the existing pinned engine:

```sh
cargo run -p featurephone-desktop --bin ph0nix
```

`FEATUREPHONE_QEMU` can select a specific `qemu-system-arm` executable. The app
also discovers `engine/qemu-system-arm` next to itself and `qemu/build/` in a
source checkout. Settings and sessions live under XDG data storage.

## Windows

Use the **UCRT64** shell in MSYS2. Install `git`, `make`, and `diffutils` from
MSYS2, plus GCC, Rust, Python/pip, Ninja, CMake, pkgconf, GLib, GTK 3, zlib,
clang, libiconv, and DTC from the matching
`mingw-w64-ucrt-x86_64-*` packages. The exact package list and build commands are in
`.github/workflows/native-apps.yml`.

Select UCRT64's Clang compiler and DLL together so Rust's bindgen uses matching
builtin headers when other LLVM installations are present:

```sh
export LIBCLANG_PATH="$(cygpath -m /ucrt64/bin)"
export CLANG_PATH="$(cygpath -m /ucrt64/bin/clang.exe)"
git config --global core.autocrlf false
git config --global core.longpaths true
git submodule update --init qemu
python frontends/build_desktop.py --jobs 2
```

Open `dist/ph0nix-windows-x86_64/ph0nix.exe`.
The script packages the native QEMU executable and its required MinGW DLLs.
QMP uses loopback TCP and file migration, including Windows path handling.
Settings and sessions retain `%LOCALAPPDATA%/FeaturePhoneEmulator` for compatibility
with earlier app versions. The Linux data-directory name is also retained.

## Android builds

Requirements: JDK 17 or newer, Python 3 with Meson 1.11.1, Android SDK platform
35/build-tools 35, NDK r29, Rust with `aarch64-linux-android`, and a native Linux
clang/lld/LLVM toolchain with CMake, Ninja and pkg-config.
The Gradle wrapper is pinned. No `cargo-ndk` installation is required.

```sh
rustup target add aarch64-linux-android
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME=/path/to/android-ndk-r29
cd frontends/android
./gradlew assembleDebug
```

`build-support/build_qemu.py` builds the pinned QEMU source and static GLib,
libiconv, libffi, PCRE2, and NDK zlib. `build-support/build_native.py` builds JNI.
Both use official NDK headers/libraries with native LLVM, allowing a Linux ARM64
build host. The generated JNI directory contains the shared JNI library and
`libdb2010_engine.so`, an executable Android PIE extracted into nativeLibraryDir.
They use 16 KiB ELF segment alignment. JNI includes NDK compiler builtins and
libunwind. Bionic lacks POSIX named shared memory; the Android build explicitly
returns ENOSYS for those APIs and uses anonymous RAM for the phone.

On Linux ARM64, Google's aapt2 binary is x86-64. Use a current aapt2 through
qemu-x86_64 and a compatible x86-64 libc, then pass its executable wrapper:

```sh
./gradlew assembleDebug -Pandroid.aapt2FromMavenOverride=/path/to/aapt2-wrapper
```

The Debian Android 11 aapt2 cannot read platform 35's resource table. Build
provenance records the NDK archive, pinned dependency hashes, QEMU revision,
the authenticated local patch series, and the generated engine hash in
`build/android/qemu/provenance.json`. Both native build scripts apply the patches
listed in `dependencies/qemu/source-lock.json` to the pinned checkout and refuse
unrelated local changes. Corresponding source includes the patch files under
`dependencies/qemu/patches/`.

## Firmware scope and performance

K750i R1DB001 CID49 has verified original startup, standby, and all five native
Settings tab screens. Profile details, ring-volume changes, Themes, and the
Calls divert-call menu have been opened. Menu access does not establish
cellular calls, Bluetooth, camera, or RF functionality.

The compatibility bundle supplies declared reset values, a derived GDFS bank,
assumed OTP identity, startup NAND, and authenticated CDA102338/36 file
provisioning. Stock FS is authenticated before applying explicit FAT/file
patches, and the result has its own pinned SHA-256. A generated 144×144 test
picture is installed as `ph0nix.png` in the existing user Pictures directory.
Original files are preserved.

After authenticating the complete MAIN container and sparse records, native
preparation changes four bytes at `0x44027130` in its private mapped copy
(`0x4C004D30` after the firmware's RAM copy). It replaces `c04991e8` with
`2c119fe5` to reload the Java context pointer after a C helper clobbers R1.
This is a firmware compatibility workaround for the Java software fallback;
QEMU does not execute Jazelle bytecode. The user's MAIN file is never rewritten.
The exact revision, source bytes and replacement are declared in
`k750-r1db001-cid49-native-compatibility.json`. The research runner retains its
original MAIN/copy contracts.

The native simulated SIM uses the test IMSI `001010123456789`, with **ph0nix**
in EF_SPN and CPHS operator-name files. The firmware accepts the card and reaches
standby with **Searching**. Network registration remains unmodeled, so the
firmware's home-screen operator name has not been established. Security/ETX,
DSP/audio, SIM and transport peers include synthetic services; cryptographic
and physical hardware equivalence are not established.

Game audio is delivered through a bounded native PCM transport to Android
AudioTrack or desktop CPAL. MIDI implements expression, stereo pan, modulation,
pitch bend, channel reset/all-notes-off, GM mode events and SP-MIDI Maximum
Instantaneous Polyphony allocation. Instruments use the declared 40-voice
triangle substitute. Original sampled instruments and DSP execution are absent.
Audio device failures are independent of firmware execution.

AMR-NB sound effects use a fixed-point OpenCORE decoder. The K750 DSP interface
passes its 12.2/10.2 kbit/s frames in padded 34-byte slots; the host validates
the slot padding before decoding the packed frame. This fixes Super Real
Tennis stopping at its first racket sound. The validation now includes a serve,
return, point score and replay with captured PCM. Aero and Puzzle also have
control actions and captured audio on the current engine.

Native apps now execute on QEMU's ordinary virtual clock, declared as
`qemu-realtime-v1`, with instruction counting disabled. This lets the ARM/Java
software fallback run without fragmenting CPU execution for every 16-byte LCD
DMA burst. Guest time follows elapsed host time and stops when the VM pauses.
Research runs retain `qemu-virtual-v1` and fixed instruction counting at shift 6;
the engine rejects mismatched clock profiles and launch options.
Native register validation and the complete MMIO access hold QEMU's device
lock while taking timestamps and updating device state. Concurrent timer
callbacks cannot invalidate a timer phase, change a validated command before
it commits, or expose a partially updated mailbox.

Presentation is scheduled at 60 Hz. QEMU sends immutable completed RGB565
panels over a separate loopback binary stream; the native worker no longer
requests each screen through QMP. The sender keeps one partial packet and the
latest pending panel, while the receiver continually drains into one latest
frame slot. Slow presentation therefore has bounded memory and does not build
a queue of stale screens. Cached QMP panels remain a negotiated fallback.
Android reuses RGB565 bitmaps and counts actual draws; desktop statistics
distinguish presentations from firmware LCD updates.

Six alternating seven-second runs from the same healthy Aero flight checkpoint
measured a median of **8.98 actual firmware LCD updates/s**, up from **3.70**,
approximately **2.42 times faster**. Median DMA timer callbacks fell from
125,716 to 75 per run, and guest time advanced at 1.001 times wall time.
Median native screen polling took 0.0044 ms, down from 9.45 ms. This scene is
silent and does not validate game sound. Measurements use the Linux ARM64 build
host; Android device performance and stable 60 FPS window presentation remain
unverified.
The game's own update cadence is separate from presentation frequency.
Raw measurements and binary/checkpoint identities are recorded in
`build/k750-performance/final-performance-report.json`.

The final engine resumed a cold-start checkpoint through language selection
and the setup wizard to standby with 150 ms keypad presses. Saving and reloading
that standby state succeeded. A resumed Tennis match ran for ten seconds with
captured PCM and another successful save. These firmware checks used the Linux
ARM64 engine; they do not establish Android app performance.

Saved phone states use a per-session file lock and an atomically replaced
manifest referencing a completed checkpoint and its TCM layout. An error state
cannot replace a healthy saved phone state. A quick relaunch
waits for the previous save off the UI thread. App updates that change the
compatibility bundle use a different session directory. Firmware versions are
kept as separate library entries; saved state is scoped to firmware and path.
Legacy native saves made with instruction counting load on the new clock with
their saved virtual timestamp preserved. New saves record `qemu-realtime-v1`;
pause/resume and save/reload tests verify that guest time never resets or moves
backwards during this transition.

## Additional manufacturers and versions

The broken Samsung E830 and N500 presets are removed from both app libraries.
Existing settings migrate to K750i without deleting imported firmware files.
Samsung, Siemens, BlackBerry, LG, and Panasonic are visible as **Planned**, with
no executable firmware profile in these apps yet.

A manufacturer name is not enough to select a CPU or hardware model. Add each
device/revision with an explicit loader (formats, companion roles and image
identities), memory/boot contract, peripheral backend, controls, and firmware
regression evidence. Register executable profiles in `native/k750::registry()`;
the Kotlin shell consumes their roles and controls through JNI. Native engines
use the host abstraction in `native/db2010-host`; registry runtimes serve the
existing Samsung boards. Planned manufacturers live in the separate catalog
and cannot be selected as working emulators.

`phone-cli run --research-board --device k750i` selects the incomplete Unicorn
research board. The working K750i apps and `db2010-host-check` use QEMU.

Third-party license texts, interface attribution, pinned source revisions and
rebuild scripts are included in the repository and native distribution.
