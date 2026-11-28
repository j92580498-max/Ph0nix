# Third-party notices

## J2ME-Loader interface base

The library structure, horizontal list layout, compact grid layout and search/view/sort/profile menus are adapted from [nikita36078/J2ME-Loader](https://github.com/nikita36078/J2ME-Loader), revision `afffdde4e677b3d5ef516e713732abc488f19fc0`, under Apache-2.0. The original layouts are `fragment_apps_list.xml`, `list_row_jar.xml`, `list_row_grid_jar.xml`, and `menu/main.xml`. Firmware import, device catalog, state settings and electrical keypad controls are added here. The MIDlet runtime is not included. The license is retained in `third_party/J2ME-Loader/LICENSE` and packaged in `assets/licenses/J2ME-Loader/LICENSE`.

## Native DB2010 engine

The APK packages the GPL-2.0-or-later DB2010 QEMU executable at the revision pinned in `dependencies/qemu/source-lock.json`, with its hash-pinned patch series. Its corresponding source is the `qemu` submodule, `dependencies/qemu/patches/`, and the build scripts in this repository. The native host is `native/db2010-host`. The APK statically links GLib 2.84.4, libiconv 1.17, libffi 3.4.6, PCRE2 10.46 and NDK zlib; source archive identities are recorded in `build/android/qemu/provenance.json`. License texts and build notices are packaged in `assets/licenses/`.

AMR-NB sound effects use the fixed-point OpenCORE AMR decoder from
[BelledonneCommunications/opencore-amr](https://github.com/BelledonneCommunications/opencore-amr),
revision `3b67218fb8efb776bcd79e7445774e02d778321d`, under Apache-2.0.
The selected decoder/common sources retain their original bytes and copyright
notices; `.cpp` files are compiled as C, as supported by upstream. Source
identities and build changes are recorded in
`qemu/hw/arm/db2010-opencore-amrnb/UPSTREAM.json` and the pinned patch series.
The upstream license is packaged in `assets/licenses/OpenCORE-AMR-Apache-2.0.txt`.

MP3 decoding uses the CC0 minimp3 sources included in the authenticated board
patch. AAC-LC decoding uses FAAD2 under GPL-2.0-or-later. Their license texts
are packaged as `assets/licenses/minimp3-CC0.txt` and
`assets/licenses/FAAD2-GPL-2.0.txt`. These codec libraries provide DSP service
responses; the original firmware controls media files and players.

The optional Android SoundFont renderer uses TinySoundFont under MIT. The
exact revision and checksum are pinned in `build-support/build_qemu.py`;
the license is packaged in `assets/licenses/TinySoundFont-MIT.txt`.

### Sony Ericsson A100 DB2020 Synth 1.0.5

SoundFont programming and compilation: **Rubén Díaz (RibénDíaz232)**,
© 2022–2026. The bank is sampled from a Sony Ericsson W580i. Original synthesis
and sound design: © 2001–2012 Sony Ericsson Mobile Communications AB.
Download it from the [author’s Musical Artifacts page](https://musical-artifacts.com/artifacts/2351).
The author allows use with attribution and asks that the file not be
redistributed on other platforms without permission.

For a personal test APK, build with `-Pph0nixTestApk=true` and
`-Pph0nixMidiSoundFont=/absolute/path/Sony_Ericsson_A100_DB2020_Synth_1.0.5.sf2`.
This installs as `org.featurephone.emulator.test`, alongside the regular app.
The repository and regular APK do not include the bank. Without it, MIDI uses
the existing triangle renderer. An in-progress track restored from a saved VM
state uses that renderer until the next track starts.

## PocketHLE Android library UI

The Android library card layout, launcher-style grid, theme, selected vector drawables and NDK linker-support pattern are adapted from [j92580498-max/PocketHLE](https://github.com/j92580498-max/PocketHLE), commit `25d13039d8f0ad3755b441215745a53e382edb1b`. PocketHLE is dual-licensed under Apache-2.0 or MIT; the corresponding license texts are in `third_party/PocketHLE/` and in the APK at `app/src/main/assets/licenses/PocketHLE/`.

## Unicorn Engine

The Android native library statically links `unicorn-engine` / `unicorn-engine-sys` version 2.1.5, which is licensed under GPL-2.0 and includes QEMU-derived code. The GPL-2.0 license text is included in the APK at `app/src/main/assets/licenses/GPL-2.0-Unicorn.txt`. The corresponding source is the pinned crate source identified by `Cargo.lock` and available from [crates.io](https://crates.io/crates/unicorn-engine-sys/2.1.5).

## Android libraries

AndroidX, Material Components and Kotlin libraries are used under their respective upstream licenses. Their exact versions are pinned in the Gradle dependency declarations.

## Native storage libraries

Raw FAT16 card images use [fatfs 0.3.6](https://crates.io/crates/fatfs/0.3.6)
under MIT. Firmware OBEX folder-listing XML uses
[quick-xml 0.41.0](https://crates.io/crates/quick-xml/0.41.0) under MIT.
Their license texts are packaged in `assets/licenses/fatfs-MIT.txt` and
`assets/licenses/quick-xml-MIT.txt`. Desktop Bluetooth loads the host's BlueZ
library through libloading; it is not bundled in the APK. Exact Rust dependency
versions and source checksums are in `Cargo.lock`.
