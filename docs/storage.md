# Native phone files and card images

The native file browser uses the running K750 firmware's PC Suite AT/OBEX service over UART0. It does not edit mounted NOR bytes on the host. The root directory is `Phone memory`, with `Other`, `Pictures`, `Sounds`, `Themes` and `Videos` provided by the firmware.

`PhoneFiles` queues one operation on a dedicated thread; the presentation and CPU threads continue while files move. Import, export, copy, move, folder creation and deletion use native OBEX requests. File transfers stream through 32 KiB buffers and have a 32 MiB size limit. A move verifies the destination's byte count and SHA-256. An imported move also waits for a healthy, durable VM checkpoint before removing the host source or reporting completion. Existing destinations require an explicit replacement request. Folder deletion requires an empty folder; recursive directory transfer is not implemented.

Before saving a native checkpoint, `Session.save()` calls a bounded file-service barrier while the firmware is still running. The barrier rejects new jobs, waits for an accepted job to finish, and sends OBEX DISCONNECT. A timeout or failed stream preserves the previous saved state. This prevents restoring a firmware session still expecting binary OBEX packets while the new host starts an ASCII AT handshake. The production firmware import/save/restore/list/export regression is recorded in `build/ph0nix-files-resume-after/run.log`; the equivalent check without this barrier failed with a receive timeout.

`delete_verified` accepts a file path, byte count and SHA-256. It checks the current source immediately before deletion, preserving a file that changed while a document picker or export was open. Card image checks hold the image lock across verification and deletion. Imported moves remain `awaiting_save` until the checkpoint is committed. Save failures and changed host sources produce an error on the original job and keep its source. After a live import checkpoint, the worker resumes the VM and reopens the file service for further requests. Card image imports already flush their filesystem changes before removing their source.

The live imported-move regression in `build/ph0nix-files-move-success/run.log` verifies retention before save, removal after save, and identical contents after a fresh restore/export. `build/ph0nix-files-move-failure/run.log` injects a checkpoint metadata write failure, verifies that the original source and previous healthy checkpoint survive, and restores the pre-import file state. The reproducible checker is `native/db2010-host/examples/phone-files-move.rs`; its optional `fail-save` argument enables the failure case. Run either case in a copied prepared-state directory.

The K750 R1DB001 production checkpoint was tested with root and folder listings, a host-to-phone import into `Phone memory/Other`, verified readback, an export with identical contents, deletion and OBEX disconnection. Evidence is in `build/ph0nix-files-roundtrip/roundtrip.log`. The reproducible native checker is `native/db2010-host/examples/phone-files.rs`:

```sh
TMPDIR=/var/tmp cargo run --locked -p db2010-host --example phone-files -- \
  qemu/build/qemu-system-arm PREPARED_STATE MENU_CHECKPOINT \
  'Phone memory/Other' SMALL_TEXT_FILE
```

It writes `ph0nix-transfer-test.txt`, exports it into the prepared-state directory, compares the contents and removes the test object from the phone. It refuses to replace an existing phone or host destination. Stop any other phone session using the same prepared state before running it.

`CardImage` creates raw FAT16 volume images from 16 to 1024 MiB. Image creation is sparse where the host filesystem supports it; operations stream file contents instead of loading the whole image into memory. The app can browse the image, create folders and import, export, copy, move or delete files. Image operations hold an exclusive file lock. Replacement uses a temporary directory entry and a backup entry so an incomplete write preserves the old destination. Portable FAT images have a 512-byte boot sector and contain a volume directly; partitioned disks and VHD container headers are not accepted.

**The K750's Memory Stick Duo controller is not implemented.** Managing a card image in the app does not mount it in the phone. `CardInfo.guest_mount_supported` and `guest_mounted` therefore remain false. Native phone file transfers work independently of the removable-card controller.

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

The driver emits TPC `2` (read long data), `3` (read short data), `4` (read registers), `7` (get interrupt), `8` (set register windows), `9` (extended command), `B` (write registers), `D` (write long data) and `E` (set command). Register-window setup and card identification, MSPro attribute data and block commands are still needed. The controller callback begins at `0x44894BAD`. An initialization argument of `0x0D` goes through clock/power helpers at `0x44A5CA08` and `0x44A5C5D8`; it is not evidence for IRQ13.

The K750 MAIN device-configuration string at `0x44433E82` contains:

```text
bps dda/MEMORY_STICK =driver_name:MEMORY_STICK ifname:MEMORY_STICK intctl:dda/intcon intline:76 intprio:31 dmach1:6
```

The configuration callback at `0x44894C50` parses `intline` and binds it through `0x447B1F5C`. The interrupt-controller setup at `0x44A64B74` subtracts compact-bank offsets 28 and 60. Thus Memory Stick compact line 76 uses tertiary bit 16 and primary cascade 30. The `dda/intcon` configuration includes line 76 in `level:9,10,11,12,52,76,77`, establishing level-triggered routing. This is separate from PDI compact line 41 (secondary bit 13).

Insertion uses a second interrupt. `dda/GPIO4` has compact line 69, tertiary bit 9 and cascade 30. The detect task at `0x448924B4` subscribes through `0x44AE2F94` with port 4, pin mask `0x04`, mode 2 and option 0. The supplied code clears the pin's direction bit at `0xF900002A`, sets polarity at `0xF900002E` according to its current level, and enables it at `0xF900002C`. The GPIO4 handler at `0x44AE240C` reads pending bits at `0xF9000030`, disables the selected pin, emits event `0x618D` and changes polarity before re-enabling it. `0x44892834` interprets GPIO42 low as inserted. The detect task consumes that event, schedules a debounce value of 20, rechecks the input, then calls `0x448926D8` to start the Memory Stick task.

`build/ph0nix-memorystick-routing.txt` records the configuration bytes and supplied K750 disassembly for these findings. Removing the empty-slot profile from a copied running checkpoint and running for five host seconds (4.29 guest seconds) produced no new controller accesses (`build/ph0nix-card-probe/probe.log`); a GPIO value change without the insertion interrupt is insufficient. The current empty-slot GPIO42 model must remain active until GPIO insertion events and the complete controller/card protocol are implemented and tested.

GitHub code searches for `MSPRO_CMD_READ_ATRB` and `MS_TPC_READ_REG` in `torvalds/linux` found useful protocol references at commit `aacb7797504183a90d66b19b1efb555cfe9b3eec`:

- [include/linux/memstick.h](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/include/linux/memstick.h) defines the TPCs, register structures, card status flags and commands. MSPro data read/write are `0x20`/`0x21`, attribute read is `0x24`, and stop is `0x25`.
- [drivers/memstick/core/mspro_block.c](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/drivers/memstick/core/mspro_block.c) implements MSPro initialization, attribute parsing and block transfers.
- [drivers/memstick/core/ms_block.c](https://github.com/torvalds/linux/blob/aacb7797504183a90d66b19b1efb555cfe9b3eec/drivers/memstick/core/ms_block.c) covers the older Memory Stick block protocol.

These sources explain the card-side protocol. The supplied MAIN establishes the DB2010 routing above; controller reset state, transfer timing and successful guest mounting remain unverified.
