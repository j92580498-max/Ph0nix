# Maintained runtime inputs

These resources are versioned emulator dependencies. They must remain usable when
research runs and derived analysis are removed. Historical source identifiers in
provenance describe evidence; the runner never follows them as file dependencies.

uart-debug-registers.json copies exactly the two UART0 register contracts from
0255 (FB06000A guest-established readback and FB060012 initializer capture) that
the0260 data transport replaced. `--uart-debug` reinstates them alongside the
verified RX0-high/RX1-low inputs, with no new response or reset-state assumption.
The effective prepared state and run manifest retain this cold-boot override;
the original experiment stays frozen and regeneration repeats the option.

The manifest preserves the MAIN/FS container identities and all 344 record
mappings for each image. The baseline preserves the direct-MAIN initial register,
memory and executable-range contract. The five configured cold-boot images are bundled under checkout fw/provisioned/.

The direct-MAIN entry is read from authenticated firmware, not the baseline:
the observed ARM reset vector `ldr pc,[pc,#0x18]` at MAIN offset0 selects the
little-endian handler literal at offset0x20. Preparation requires a word-aligned
handler inside supplied MAIN code; sparse K750 gaps and expanded W800 storage
tails are excluded. Unsupported vector layouts remain guarded. Container inputs
are unpacked into their mapped records before reading the vector.

execution-contract.json promotes the already authenticated original-code copy
mappings retained in the sealed 0234-mma-fresh-normal-v2 run. It preserves both
approved source/destination ranges and their exact byte hashes, plus chronological
copy provenance. Expected bytes are regenerated from authenticated MAIN; they do
not seed RAM. QEMU still requires known guest-written instruction bytes and stops
on changed, unknown or unapproved execution. This records an emulator execution
contract, not a new measurement of handset behavior.

boot.json is the standalone default, copied from verified research0271: exact
w800-configured NOR/NAND exports with the same0268 device contracts. Native
saved language/settings persist in storage; no CPU/device checkpoint is restored.
Research owns numbered configurations and its relative current.json pointer. The
authored customize-minimal-english.xml supplies the one-file English-only,
provider-free package. The A0 vibration latch removes the PMIC guard exposed
by this input; customize-minimal-english-vibrator.xml retains0267's historical
profile workaround.
The XML resources are self-contained; their deterministic host ZIP/FS generation
and native evidence are documented in the root minimal-customization investigation.
0234 and0235 remain explicitly selected research profiles. Other historical experiments
remain with the research repository; test-only profiles belong in tests/resources.

MAIN storage preparation authenticates the original payload, then pads short
inputs with known erased FF to 0x45400000 (20,840,448 bytes). Expanded raw MAIN
inputs may supply tail data, while original code bounds and copy proofs remain
unchanged. Raw NAND page/spare overrides establish all supplied bytes and keep
cold controller/bookkeeping defaults; flash/ images contain no runtime state.

The separate k750-r1db001-cid49 baseline/manifest and experiment0269 authenticate
cataloged K750 MAIN/FS records without flattening sparse gaps. The original
prefix profile excludes RAM execution; 0270 added the authenticated 36,008-byte
original-copy contract. The maintained RAM contract also covers a verified
4,880-byte copy from `0x4402B0B0` to `0x4C008CB0`; the intervening eight-byte gap
remains excluded, and proof bytes never seed RAM. The sparse prefix profiles leave GDFS/tails unknown.
The standalone k750-r1db001-cid49-boot.json profile selects the RAM baseline with
shared DB2010 controller/register contracts, RTC, TIMER1 IRQ routing, the declared
polled PMIC ADC sample `0x82` and unseeded Samsung `EC/35` NAND reset/ID/status.
Its timer frequency, ADC input and fitted flash identity are explicit compatibility
assumptions. NAND supplies no MAIN/spare bytes and K750 preparation rejects
additional NAND storage/protocol fields. The profile has no research-file
dependency. The guarded prefix reaches 16,697,595 instructions and 113 timer IRQs,
then stops at unknown NOR `0x45E3FFFE` beyond authenticated FS end `0x45E00404`.
This is an original two-byte read at the end of a 256 KiB FS block. QEMU
display/progress counters report zero LCD frames; see docs/k750i.md.
k750-r1db001-cid49-source.json pins a public source commit for the authenticated
MAIN/FS pair and a hash-pinned firmware.center GDFS backup.
`development.firmware init k750i` fetches missing inputs;
`check k750i` verifies existing images without network access. Explicit K750
MAIN/FS paths retain the same complete-container and sparse-payload checks.
The original numbered research configurations remain in research.

`k750-r1db001-cid49-screen.json` is the `--k750i` startup-screen profile.
It preserves authenticated MAIN/FS bytes and regenerated RAM-copy proofs, adds
an explicit erased FS remainder with four empty-block footers, reconstructs
seven GDFS banks from exact logical backup payloads, and initializes NAND page
and spare storage to assumed erased FF. Generated inputs use the ordinary
content-addressed cache; no investigation directory is a runtime dependency.
Native K750 upload records identify the ten DSP tail pairs initialized to zero.
K750 PMIC calibration writes and power/reset-record inputs are declared in the
profile. The optional SIMIF setup reset value0 permits the original setup
read/modify/write sequence. The optional F903 hash FIFO runs immediate SHA-1
arithmetic; mode4/KGEN equivalence is unestablished. Identified K750 ROM init
callers use synthetic contexts while original MAIN performs FIFO update/final.
The shared FM sink accepts the observed K750 startup frame
`0080000bd20a0040100000` at wire address `0x20`, with no RF/tuning behavior.
The frame evidence includes the original Sony Ericsson logo followed by a native
“Please wait” screen and progress-bar updates. Browser pixels match the captured
LCD panel exactly; full UI boot remains unestablished. `--k750i-prefix` selects
the unchanged sparse boot resource.

The native apps use `k750-r1db001-cid49-native-compatibility.json` to declare
an exact four-byte Java fallback R1 reload in their private MAIN mapping after
the original container is authenticated. Original user files and the research
runner's MAIN/copy contracts remain unchanged. `ph0nix-puzzle.png` is a generated
144×144 test picture; native bundle construction installs it in the existing
`tpa/user/image` directory without replacing stock pictures. The bundle also
selects the `ph0nix-test-sim` profile, with a valid test IMSI and ph0nix SPN/CPHS
name. The SIM is accepted by the firmware; cellular registration is absent.

`non-provisioned-boot.json` selects original0268 storage under `fw/non-provisioned/`
for the explicit `--non-provisioned` CLI option. Default boot uses provisioned images.
