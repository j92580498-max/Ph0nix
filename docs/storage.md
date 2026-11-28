# Native phone files and card images

The native file browser uses the running K750 firmware's PC Suite AT/OBEX service over UART0. It does not edit mounted NOR bytes on the host. The root directory is `Phone memory`, with `Other`, `Pictures`, `Sounds`, `Themes` and `Videos` provided by the firmware.

On a normal stop, K750i saves internal flash in `phone-memory.k750mem`, even when resume is disabled. The file contains MAIN, FS, GDFS, NAND main and NAND spare, with a version header and SHA-256 for each image. It excludes CPU state, RAM and Memory Stick. A cold boot verifies the images and loads them with fresh flash controllers; the firmware may still show `Please wait`. Legacy storage alongside a VM checkpoint can be read and is converted on the next successful save. A rejected PMIC write can stop the VM while it still holds committed flash changes. If no phone file transfer was interrupted, the host saves those flash bytes without saving the faulted session; the next launch cold boots. Data that the firmware had not yet written to flash cannot be recovered. Forced termination does not trigger a save.

In Android, every new K750i entry gets its own `phone-memory.k750mem` when MAIN and FS are imported. Adding the same pair again creates a fresh phone. Existing entries keep their saved data in their original location.

In Android, open **Library**, open the K750i entry's **⋮** menu and choose **Export internal memory…**. Save the file to Android Downloads or another folder outside the emulated phone. The app prepares and verifies the backup before opening the save picker, then reopens the saved copy and compares it to the source. If the copy fails, it asks the document provider to remove the unfinished file. **Import internal memory…** verifies the complete file before replacing the selected phone's saved storage. The next launch starts with the imported flash and fresh CPU state, even if resume is enabled. A corrupt or incomplete backup is rejected before the current memory file is replaced. The matching K750i MAIN/FS pair is still required to prepare the phone for boot.

With resume enabled, the VM checkpoint is kept separately. Its metadata refers to a specific flash generation so an older checkpoint cannot resume over newer memory. With resume disabled, automatic saves and live imported moves write the flash file without creating a checkpoint.

On a runner with a built DB2010 engine and a disposable copied firmware workspace, `cargo run --locked -p db2010-host --example phone-files-resume -- ENGINE COPIED_STATE CHECKPOINT cold-boot` checks export, import, rejection of corruption and byte-for-byte readback after a cold boot. Omitting `cold-boot` runs the VM-resume regression. `--fresh NEW_DATA_DIR MAIN FS` creates two phones from the same MAIN/FS paths, verifies separate memory files, and checks that only an explicit import changes the second phone.

Run **K750i cold boot and Android APK** manually from `main` in the original
`j92580498-max/Ph0nix` repository. It uses the owner's Linux x64 self-hosted
runner labeled `ph0nix-owner`. The workflow sets up stable Rust for the runner
user. The runner needs Python 3 with `venv` and these build packages (Debian/Ubuntu
names): `build-essential`, `cmake`, `ninja-build`,
`pkg-config`, `python3-venv`, `clang`, `libclang-dev`, `libglib2.0-dev`,
`libpixman-1-dev`, `libfdt-dev` and `zlib1g-dev`. It builds the pinned engine,
fetches the authenticated MAIN/FS, and runs the example with
`--fresh NEW_DATA_DIR MAIN FS` in a temporary directory. Success prints
`{"save_cold_boot_roundtrip":"passed"}`. If the initial guest setup prevents the
file service from accepting commands, finish setup and use the copied checkpoint
form above to start from a known working handset state.

`PhoneFiles` queues one operation on a dedicated thread; the presentation and CPU threads continue while files move. Import, export, copy, move, folder creation and deletion use native OBEX requests. File transfers stream through 32 KiB buffers and have a 32 MiB size limit. A move verifies the destination's byte count and SHA-256. An imported move also waits for durable internal storage before removing the host source or reporting completion. Existing destinations require an explicit replacement request. Folder deletion requires an empty folder; recursive directory transfer is not implemented.

Before saving internal memory or a VM checkpoint, the native host calls a bounded file-service barrier while the firmware is still running. The barrier rejects new jobs, waits for an accepted job to finish, and sends OBEX DISCONNECT. A timeout or failed stream preserves the previous saved state. This prevents restoring a firmware session still expecting binary OBEX packets while the new host starts an ASCII AT handshake. The production firmware import/save/restore/list/export regression is recorded in `build/ph0nix-files-resume-after/run.log`; the equivalent check without this barrier failed with a receive timeout.

`delete_verified` accepts a file path, byte count and SHA-256. It checks the current source immediately before deletion, preserving a file that changed while a document picker or export was open. Card image checks hold the image lock across verification and deletion. Imported moves remain `awaiting_save` until the save is committed. Save failures and changed host sources produce an error on the original job and keep its source. After a live import save, the worker resumes the VM and reopens the file service for further requests. Card image imports already flush their filesystem changes before removing their source.

The live imported-move regression in `build/ph0nix-files-move-success/run.log` verifies retention before save, removal after save, and identical contents after a fresh restore/export. `build/ph0nix-files-move-failure/run.log` injects a checkpoint metadata write failure, verifies that the original source and previous healthy checkpoint survive, and restores the pre-import file state. The reproducible checker is `native/db2010-host/examples/phone-files-move.rs`; its optional `fail-save` argument enables the failure case. Run either case in a copied prepared-state directory.

The K750 R1DB001 production checkpoint was tested with root and folder listings, a host-to-phone import into `Phone memory/Other`, verified readback, an export with identical contents, deletion and OBEX disconnection. Evidence is in `build/ph0nix-files-roundtrip/roundtrip.log`. The reproducible native checker is `native/db2010-host/examples/phone-files.rs`:

```sh
TMPDIR=/var/tmp cargo run --locked -p db2010-host --example phone-files -- \
  qemu/build/qemu-system-arm PREPARED_STATE MENU_CHECKPOINT \
  'Phone memory/Other' SMALL_TEXT_FILE
```

It writes `ph0nix-transfer-test.txt`, exports it into the prepared-state directory, compares the contents and removes the test object from the phone. It refuses to replace an existing phone or host destination. Stop any other phone session using the same prepared state before running it.

`CardImage` creates raw FAT16 volume images from 16 to 1024 MiB. Image creation is sparse where the host filesystem supports it; operations stream file contents instead of loading the whole image into memory. The app can browse the image, create folders and import, export, copy, move or delete files. Image operations hold an exclusive file lock. Replacement uses a temporary directory entry and a backup entry so an incomplete write preserves the old destination. Portable FAT images have a 512-byte boot sector and contain a volume directly; partitioned disks and VHD container headers are not accepted.

### Experimental K750i Memory Stick PRO Duo

On desktop, open **Card images**, create or open a FAT16 `.img`, and select **Use for the next K750i boot**. On Android, choose a card image when starting a K750i from the library, or select **Create a card and start…** and pick a size from 16 MiB to 1 GiB. The engine attaches the image before the CPU starts, so the firmware can detect it during boot. Stop the phone before changing the image or editing it from the host. To reuse a card after a cold boot, select the same `.img`. With a card inserted, a normal stop gives the firmware 30 seconds of emulated time to finish card writes, then saves internal storage without a VM checkpoint. Wait for the app to finish saving before reopening or editing the image.

On Android 8.0 or later, open **Files → ph0nix → [phone name]**. To copy a file into a phone, start from the source file and choose **Copy to… → ph0nix → [phone name]**, then open the exact destination folder and tap **Copy**. The limit is 32 MiB per file, subject to the free space on the destination.

- **Internal memory** shows the K750i user folders. Return to the ph0nix library and wait for the phone to finish saving before opening them. The phone still holds its memory while paused or in the background. Files go to the chosen folder directly; you can see them on the next boot.
- **Memory Stick (memory card)** lists card images by name and size. Files also shows total, used and free space from FAT allocation; the launch picker shows the nominal image size. Cards are shared across K750i machines. Create or import a card from the launch menu if the list is empty. Stop the phone using a card and wait for it to finish saving before editing the card from Files. Start the phone with the same card to see the files.

To export an entire card on Android, stop the phone and wait for saving to finish. In **Library → K750i entry → ⋮ → Export memory card…**, choose an image and save the `.img` to Downloads or another folder outside ph0nix. Export needs free space for a temporary copy in the app cache as well as the saved image. The app verifies the saved image and asks the document provider to remove it if the export fails. To reuse the exported card, choose **Import an image and start…** when launching K750i.

If your file manager does not show ph0nix, use Android's system Files app.

Firmware compatibility is checked by the `K750i Memory Stick storage` workflow on CT100. It exercises the stock R1DB001 firmware, including file listing, writeback to the raw image, and reopening the file after a cold boot.

### Measuring Back to library

Capture `adb logcat -s Ph0nixSave:I` on the same Android device for four cases: no card with Resume off, no card with Resume on, an idle inserted card, and exit immediately after a write to an inserted card. Compare `save_total` and the individual save phases with `library_pre_draw`. The `storage_notifications_complete` line measures provider notification, not a successful Files read. Separately open Files and copy a known file out to check its bytes, then copy another file in and verify it after a cold boot. A run where storage notifications already completed before Back is not a fresh stop measurement.

For a disposable card workspace on a Linux host, run `cargo run --locked -p db2010-host --example memorystick-probe -- ENGINE WORKSPACE MAIN FS 32`. Repeat with a new workspace and `64`. Append `idle` to save without a guest write. The write case saves as soon as the firmware import completes and checks the bytes in both the stopped raw image and the cold-booted firmware. These host probes do not measure Android latency.

## Controller evidence for further implementation

The supplied K750 MAIN has its low-level controller driver at `0x4489420C` through `0x44894Cxx`. Static disassembly identifies the following accesses. These are firmware observations; reset values, timing and supported inserted-card behavior are not inferred from them.

| Address | Observed use |
| --- | --- |
| `0xFC010000` | Halfword command: high nibble is the Memory Stick TPC; remaining bits contain the byte count. |
| `0xFC010008` | Control read/modify/write; bit `0x20` brackets transfers. |
| `0xFC01000C` | Halfword status with write-back acknowledgement; the handler checks `0x80` completion, `0x02` error and `0x40` card event. |
| `0xFC010010` | Halfword event mask/control; the driver toggles `0x80`. |
| `0xFC01001C` | FIFO access mode: bits `0..1` select read width and bits `4..5` select write width. |
| `0xFC010020` | Word reads from the receive FIFO. A mode of zero extracts one low byte; mode three extracts four bytes. |
| `0xFC010024` | Word writes to the transmit FIFO; width follows bits `4..5` of the FIFO mode. |
| `0xFC01002C` | Halfword FIFO counts: low byte receive count; high byte transmit count, with a 16-byte transmit capacity assumed by the driver. |

The driver emits TPC `2` (read long data), `3` (read short data), `4` (read registers), `7` (get interrupt), `8` (set register windows), `9` (extended command), `B` (write registers), `D` (write long data) and `E` (set command). The experimental model implements the register windows, MS PRO identification, attribute reads and the sector read/write protocol. The controller callback begins at `0x44894BAD`. An initialization argument of `0x0D` goes through clock/power helpers at `0x44A5CA08` and `0x44A5C5D8`; it is not evidence for IRQ13.

The K750 MAIN device-configuration string at `0x44433E82` contains:

```text
bps dda/MEMORY_STICK =driver_name:MEMORY_STICK ifname:MEMORY_STICK intctl:dda/intcon intline:76 intprio:31 dmach1:6
```

The configuration callback at `0x44894C50` parses `intline` and binds it through `0x447B1F5C`. The interrupt-controller setup at `0x44A64B74` subtracts compact-bank offsets 28 and 60. Thus Memory Stick compact line 76 uses tertiary bit 16 and primary cascade 30. The `dda/intcon` configuration includes line 76 in `level:9,10,11,12,52,76,77`, establishing level-triggered routing. This is separate from PDI compact line 41 (secondary bit 13).

Insertion uses a second interrupt. `dda/GPIO4` has compact line 69, tertiary bit 9 and cascade 30. The detect task at `0x448924B4` subscribes through `0x44AE2F94` with port 4, pin mask `0x04`, mode 2 and option 0. The supplied code clears the pin's direction bit at `0xF900002A`, sets polarity at `0xF900002E` according to its current level, and enables it at `0xF900002C`. The GPIO4 handler at `0x44AE240C` reads pending bits at `0xF9000030`, disables the selected pin, emits event `0x618D` and changes polarity before re-enabling it. `0x44892834` interprets GPIO42 low as inserted. The detect task consumes that event, schedules a debounce value of 20, rechecks the input, then calls `0x448926D8` to start the Memory Stick task.

`build/ph0nix-memorystick-routing.txt` records the configuration bytes and supplied K750 disassembly for these findings. Removing the empty-slot profile from a copied running checkpoint and running for five host seconds (4.29 guest seconds) produced no new controller accesses (`build/ph0nix-card-probe/probe.log`); a GPIO value change without the insertion interrupt is insufficient. The apps present the card at boot so the firmware can sample the GPIO42 level directly. Insertion while powered on remains a developer probe.

GitHub code searches for `MSPRO_CMD_READ_ATRB` and `MS_TPC_READ_REG` in `torvalds/linux` found useful protocol references at commit `aacb7797504183a90d66b19b1efb555cfe9b3eec`:

- [include/linux/memstick.h](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/include/linux/memstick.h) defines the TPCs, register structures, card status flags and commands. MSPro data read/write are `0x20`/`0x21`, attribute read is `0x24`, and stop is `0x25`.
- [drivers/memstick/core/mspro_block.c](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/drivers/memstick/core/mspro_block.c) implements MSPro initialization, attribute parsing and block transfers.
- [drivers/memstick/core/ms_block.c](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/drivers/memstick/core/ms_block.c) covers the older Memory Stick block protocol.

These sources explain the card-side protocol. The supplied MAIN establishes the DB2010 routing above; controller reset state, transfer timing and successful guest mounting remain unverified.
