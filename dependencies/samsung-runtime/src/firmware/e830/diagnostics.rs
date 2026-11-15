//! Passive, bounded diagnostics for this firmware revision.
use super::*;

pub(super) fn state_dispatch_kind(pc: u64, table: u32) -> (bool, bool) {
    (
        pc == STATE_TABLE_DISPATCH_PC && table == KEYBOARD_INIT_TABLE,
        pc == STATE_TABLE_DISPATCH_PC && table == STANDBY_LOOK_TABLE,
    )
}

// Keep diagnostics away from MMIO: even a read may consume peripheral data.
pub(super) fn diagnostic_ram_read(uc: &Unicorn<'_, E830State>, address: u64, size: usize) -> Option<Vec<u8>> {
    let end = address.checked_add(size as u64)?;
    if address < u64::from(RAM_BASE) || end > u64::from(RAM_BASE) + RAM_SIZE as u64 {
        return None;
    }
    uc.mem_read_as_vec(address, size).ok()
}

// Static MCFN descriptors live in the immutable CLA mapping, while their
// command objects live in RAM. Keep this helper away from external flash and
// every MMIO window; an unsuccessful read simply produces no diagnostic data.
pub(super) fn diagnostic_code_or_ram_read(
    uc: &Unicorn<'_, E830State>,
    address: u64,
    size: usize,
) -> Option<Vec<u8>> {
    let end = address.checked_add(size as u64)?;
    let in_ram = address >= u64::from(RAM_BASE)
        && end <= u64::from(RAM_BASE) + RAM_SIZE as u64;
    let in_cla = address >= u64::from(FLASH_BASE) && end <= EXTERNAL_FLASH_WINDOW_BASE;
    (in_ram || in_cla).then(|| uc.mem_read_as_vec(address, size).ok()).flatten()
}

pub(super) fn diagnostic_ram_u32(uc: &Unicorn<'_, E830State>, address: u64) -> Option<u32> {
    diagnostic_ram_read(uc, address, 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
}

pub(super) fn diagnostic_ram_u8(uc: &Unicorn<'_, E830State>, address: u64) -> Option<u8> {
    diagnostic_ram_read(uc, address, 1).and_then(|bytes| bytes.first().copied())
}

pub(super) fn diagnostic_ram_u16(uc: &Unicorn<'_, E830State>, address: u64) -> Option<u16> {
    diagnostic_ram_read(uc, address, 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_le_bytes)
}

/// Bounded watch snapshots must not consume device registers or FIFO data.
pub(super) fn diagnostic_watch_words(uc: &Unicorn<'_, E830State>, address: u64, size: usize) -> Option<String> {
    diagnostic_ram_read(uc, address, size.clamp(4, 128)).map(|bytes| {
        bytes.chunks_exact(4).map(|word| {
            format!("{:08x}", u32::from_le_bytes(word.try_into().unwrap()))
        }).collect::<Vec<_>>().join(",")
    })
}

pub(super) fn menu_input_stage(address: u64) -> &'static str {
    match address {
        0x215a_6236 => "hkbd-to-edp",
        0x20d7_5404 => "edp-pipe-entry",
        0x20d7_547c => "edp-direct-route",
        0x20d7_54da => "edp-alternate-route",
        0x20d7_6304 => "edp-diffuse-entry",
        0x2055_9318 => "edp-no-diff-predicate",
        0x2055_933a => "edp-no-diff-true",
        0x2055_933e => "edp-no-diff-false",
        0x20d7_631e => "edp-diffuse-predicate-result",
        0x20d7_632a => "edp-diffuse-drop",
        0x20d7_6334 => "edp-diffuse-special-route",
        0x20d7_634e => "edp-diffuse-forward",
        0x2015_6b34 => "mmi-standby-entry",
        0x2119_fe50 => "mmi-root-router",
        0x2015_6e80 => "mmi-set-keyboard",
        0x2015_6eda => "mmi-kbd-config-send-initial",
        0x2015_6f72 => "mmi-kbd-config-send-profile",
        0x2015_741a => "mmi-kbd-config-send-late-profile",
        0x2015_708c => "mmi-keyboard-profile-4",
        0x2015_70e6 => "mmi-keyboard-profile-5",
        0x215a_5fc0 => "hkbd-key-record-lookup",
        0x2158_2028 => "edp-kbd-config-receive",
        0x2158_2042 => "edp-to-hkbd-config",
        0x20d9_221c => "hkbd-config-receive",
        0x20d9_225a => "hkbd-config-applied",
        0x2014_0c1e => "hkbd-press-mailbox-count-compare",
        0x2014_0c3e => "hkbd-press-mailbox-count-decision",
        0x2014_0e9c => "hkbd-key-info-processing",
        0x2014_0ea6 => "hkbd-key-info-record-type",
        0x2014_0ebe => "hkbd-key-info-config-value",
        0x2014_0ec4 => "hkbd-key-info-config-accepted",
        0x2014_0edc => "hkbd-mailbox-count-compare",
        0x2014_0ef0 => "hkbd-mailbox-count-decision",
        0x2014_0efe => "hkbd-key-info-send",
        0x2014_0f1a => "hkbd-key-info-config-rejected",
        0x215a_6266 => "hkbd-key-info-to-edp",
        0x2055_79b8 => "edp-key-info-entry",
        0x2055_85e4 => "edp-key-info-build",
        0x2055_864c => "edp-key-info-forward",
        0x211a_08b6 => "mmi-key-pressed-consumer",
        0x211a_27e6 => "mmi-key-pressed-direction",
        0x211a_57c0 => "mmi-key-filter-entry",
        0x211a_593a => "mmi-key-filter-up",
        0x211a_599e => "mmi-key-filter-result",
        0x200d_4cc4 => "mcc-getkey-accept-reject-config",
        0x2111_2a6c => "mcc-getkey-s-config",
        0x2153_656c => "mcc-getkey-incall-config",
        0x200d_4d94 => "mcc-getkey-accept-reject-s-filter-call",
        0x200d_4d9c => "mcc-getkey-accept-reject-s-filter-decision",
        0x200d_4dcc => "mcc-getkey-accept-reject-s-rejected",
        0x2111_2b84 => "mcc-getkey-s-filter-call",
        0x2111_2b8c => "mcc-getkey-s-filter-decision",
        0x2153_66a0 => "mcc-getkey-incall-s-filter-call",
        0x2153_66a8 => "mcc-getkey-incall-s-filter-decision",
        0x2111_2b96 => "mcc-getkey-s-ui-dispatch",
        0x215b_8a30 => "ui-dispatch-up",
        0x2159_dd48 => "ui-command-entry",
        0x2159_dd60 => "ui-command-early-return",
        0x2159_de34 => "ui-command-action",
        0x2159_defe => "ui-command-action-alternate",
        0x214d_93e4 => "ui-command-action-hook",
        0x2098_9e58 => "hkbd-clear-all-config",
        0x208e_51ac => "keypad-irq16-entry",
        0x208a_74d0 => "keypad-release-resolve",
        0x208a_703c => "keypad-release-send",
        0x2014_0c68 => "hkbd-press-gate-record",
        0x2014_0d54 => "hkbd-release-short-alternate",
        _ => "unknown",
    }
}

pub(super) fn diagnostic_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn key_filter_caller(lr: u32) -> &'static str {
    match lr & !1 {
        0x200d_4d98 => "mcc-getkey-accept-reject-s",
        0x2111_2b88 => "mcc-getkey-s",
        0x2153_66a4 => "mcc-getkey-incall-s",
        _ => "other",
    }
}

pub(super) fn key_filter_context(lr: u32, r4: u32, r5: u32, r6: u32) -> u32 {
    match lr & !1 {
        0x200d_4d98 => r6,
        0x2111_2b88 => r4,
        0x2153_66a4 => r5,
        _ => 0,
    }
}

pub(super) fn trace_menu_input_stage(uc: &mut Unicorn<'_, E830State>, address: u64, total: u64) {
    let enabled = {
        let state = uc.get_data();
        state.trace_menu_input
            && total >= state.trace_menu_input_after
            && state.trace_menu_input_remaining > 0
            && MENU_INPUT_TRACE_PCS.contains(&address)
    };
    if !enabled { return; }

    let r0 = uc.reg_read(RegisterARM::R0).unwrap_or(0) as u32;
    let r1 = uc.reg_read(RegisterARM::R1).unwrap_or(0) as u32;
    let r2 = uc.reg_read(RegisterARM::R2).unwrap_or(0) as u32;
    let r3 = uc.reg_read(RegisterARM::R3).unwrap_or(0) as u32;
    let r4 = uc.reg_read(RegisterARM::R4).unwrap_or(0) as u32;
    let r5 = uc.reg_read(RegisterARM::R5).unwrap_or(0) as u32;
    let r6 = uc.reg_read(RegisterARM::R6).unwrap_or(0) as u32;
    let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0) as u32;
    let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0) as u32;

    let object = match address {
        0x215a_6236 | 0x215a_6266 => r3,
        0x20d7_5404 | 0x2015_6b34 => r0,
        0x20d7_547c | 0x20d7_54da | 0x20d7_631e | 0x20d7_632a
        | 0x20d7_6334 | 0x20d7_634e => r5,
        0x20d7_6304 => r1,
        0x2015_6eda | 0x2015_6f72 | 0x2015_741a | 0x2158_2042 => r3,
        0x2015_708c | 0x2015_70e6 | 0x215a_5fc0 => r0,
        0x2158_2028 | 0x20d9_221c => r0,
        0x20d9_225a => r5,
        0x2055_79b8 | 0x2055_85e4 => r0,
        0x2055_864c => r1,
        0x211a_08b6 | 0x211a_27e6 => r4,
        0x211a_57c0 => key_filter_context(lr, r4, r5, r6),
        0x200d_4cc4 | 0x2111_2a6c | 0x2153_656c => r0,
        _ => 0,
    };
    let object_bytes = if object == 0 {
        "-".into()
    } else {
        diagnostic_ram_read(uc, u64::from(object), 0x1c)
            .map(|bytes| diagnostic_hex(&bytes))
            .unwrap_or_else(|| "unreadable".into())
    };
    let indirect = if matches!(address, 0x20d7_5404 | 0x2015_6b34 | 0x2055_79b8 | 0x2055_85e4 | 0x200d_4cc4 | 0x2111_2a6c | 0x2153_656c | 0x20d9_221c) {
        diagnostic_ram_u32(uc, u64::from(object))
            .and_then(|pointer| diagnostic_code_or_ram_read(uc, u64::from(pointer), 0x20)
                .map(|bytes| format!("{pointer:08x}:{}", diagnostic_hex(&bytes))))
            .unwrap_or_else(|| "-".into())
    } else {
        "-".into()
    };
    let keyboard_profile = diagnostic_ram_u8(uc, 0x3040_067c).unwrap_or(u8::MAX);
    let pending_message = diagnostic_ram_u32(uc, 0x3001_4df4).unwrap_or(u32::MAX);
    let filter_key = match address {
        0x211a_57c0 | 0x200d_4d94 | 0x2111_2b84 | 0x2153_66a0 => r0,
        0x211a_593a => 0x14,
        _ => u32::MAX,
    };
    let filter_key_info = if matches!(
        address,
        0x211a_57c0 | 0x211a_593a | 0x211a_599e
            | 0x200d_4d94 | 0x2111_2b84 | 0x2153_66a0
    ) {
        r1
    } else {
        u32::MAX
    };
    let (filter_mask_a, filter_mask_b, filter_key_offset) = match address {
        0x211a_57c0 | 0x200d_4d94 | 0x2111_2b84 | 0x2153_66a0 => (
            r2,
            r3,
            diagnostic_ram_u32(uc, u64::from(sp)).unwrap_or(u32::MAX),
        ),
        0x211a_593a | 0x211a_599e => (r2, r6, r5),
        _ => (u32::MAX, u32::MAX, u32::MAX),
    };
    let filter_result = if matches!(address, 0x211a_599e | 0x200d_4d9c | 0x2111_2b8c | 0x2153_66a8) {
        r0
    } else {
        u32::MAX
    };
    let up_allowed_a = if filter_mask_a != u32::MAX {
        (filter_mask_a >> 17) & 1
    } else {
        u32::MAX
    };
    let up_allowed_b = if filter_mask_b != u32::MAX {
        (filter_mask_b >> 17) & 1
    } else {
        u32::MAX
    };
    let lk_state = diagnostic_ram_read(uc, 0x3036_c108, 0x20)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let gate = diagnostic_ram_u8(uc, MENU_INPUT_KEY_BLOCK_CANDIDATE).unwrap_or(u8::MAX);
    let gate_context = diagnostic_ram_read(uc, 0x3048_5708, 4)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let editor_mode = diagnostic_ram_u8(uc, 0x302e_7540).unwrap_or(u8::MAX);
    let editor_gate = diagnostic_ram_u8(uc, 0x302e_74e8).unwrap_or(u8::MAX);
    let pipe_gate = diagnostic_ram_u8(uc, 0x3001_4df1).unwrap_or(u8::MAX);
    let hkbd_config = diagnostic_ram_read(uc, 0x301c_e2c8, 0x1b)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let hkbd_active = diagnostic_ram_read(uc, 0x301c_e2f4, 0x10)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let hkbd_key_record = diagnostic_ram_u32(uc, 0x301c_e2fc)
        .map(|record| {
            let bytes = diagnostic_ram_read(uc, u64::from(record), 8)
                .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
            format!("{record:08x}:{bytes}")
        }).unwrap_or_else(|| "?".into());
    let hkbd_record_type = diagnostic_ram_u32(uc, 0x301c_e2fc)
        .and_then(|record| diagnostic_ram_u16(uc, u64::from(record) + 2))
        .unwrap_or(u16::MAX);
    let hkbd_record_index = diagnostic_ram_u32(uc, 0x301c_e2fc)
        .and_then(|record| diagnostic_ram_u16(uc, u64::from(record) + 4))
        .unwrap_or(u16::MAX);
    let hkbd_record_key = diagnostic_ram_u32(uc, 0x301c_e2fc)
        .and_then(|record| diagnostic_ram_u16(uc, u64::from(record) + 6))
        .unwrap_or(u16::MAX);
    let hkbd_config_index = match hkbd_record_type {
        0 => hkbd_record_index,
        3 => 3,
        _ => u16::MAX,
    };
    let hkbd_config_value = if hkbd_config_index == u16::MAX {
        u8::MAX
    } else {
        diagnostic_ram_u8(uc, 0x301c_e2c8 + u64::from(hkbd_config_index))
            .unwrap_or(u8::MAX)
    };
    let hkbd_mailbox_processes = diagnostic_ram_read(uc, 0x301c_e2bd, 3)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let hkbd_mailbox_counts = diagnostic_ram_read(uc, 0x301c_e2e6, 6)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let hkbd_mailbox_limits = diagnostic_code_or_ram_read(uc, 0x20a8_e91c, 3)
        .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
    let hkbd_mailbox_records = diagnostic_ram_u32(uc, 0x3090_0190)
        .map(|base| {
            [0x23u32, 0x3b, 0x50].into_iter().map(|process| {
                let address = u64::from(base) + u64::from(process) * 0x38;
                let record = diagnostic_ram_read(uc, address, 0x38)
                    .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
                format!("{process:02x}@{address:08x}:{record}")
            }).collect::<Vec<_>>().join(",")
        }).unwrap_or_else(|| "?".into());
    let hkbd_process_directory = [0x23u64, 0x3b, 0x50].into_iter().map(|process| {
        let address = 0x3002_eaac + process * 8;
        let entry = diagnostic_ram_read(uc, address, 8)
            .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
        format!("{process:02x}@{address:08x}:{entry}")
    }).collect::<Vec<_>>().join(",");
    let current_rtos_task = diagnostic_ram_u32(uc, 0x3090_0194)
        .map(|task| {
            let record = diagnostic_ram_read(uc, u64::from(task), 0x38)
                .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into());
            format!("{task:08x}:{record}")
        }).unwrap_or_else(|| "?".into());
    let rejected_up_override = diagnostic_ram_u8(uc, 0x3040_13a7).unwrap_or(u8::MAX);
    let filter_caller = if matches!(address, 0x211a_57c0) {
        key_filter_caller(lr)
    } else {
        "-"
    };
    let mcc_base = if matches!(address, 0x200d_4cc4 | 0x2111_2a6c | 0x2153_656c) {
        r0
    } else if address == 0x211a_57c0 {
        key_filter_context(lr, r4, r5, r6)
    } else if matches!(address, 0x200d_4d94 | 0x200d_4d9c | 0x200d_4dcc) {
        r6
    } else if matches!(address, 0x2111_2b84 | 0x2111_2b8c) {
        r4
    } else if matches!(address, 0x2153_66a0 | 0x2153_66a8) {
        r5
    } else {
        0
    };
    let mcc_context = if mcc_base != 0 {
        diagnostic_ram_read(uc, u64::from(mcc_base) + 0x180, 0x40)
            .map(|bytes| diagnostic_hex(&bytes)).unwrap_or_else(|| "?".into())
    } else {
        "-".into()
    };
    let ui_command_gates = if matches!(address, 0x2159_dd48 | 0x2159_dd60 | 0x2159_de34 | 0x2159_defe | 0x214d_93e4) {
        [
            0x3022_18dc, 0x3022_19c7, 0x3045_0abf, 0x3022_3c0e,
            0x3022_3c19, 0x3022_3c21, 0x3040_1393, 0x3040_139d,
            0x3040_13b8, 0x302e_74f7,
        ].into_iter().map(|gate_address| {
            let value = diagnostic_ram_u8(uc, gate_address).unwrap_or(u8::MAX);
            format!("{gate_address:08x}:{value:02x}")
        }).collect::<Vec<_>>().join(",")
    } else {
        "-".into()
    };
    eprintln!(
        "[e830-menu-input] stage={} pc=0x{address:08x} insns={total} r0=0x{r0:08x} r1=0x{r1:08x} r2=0x{r2:08x} r3=0x{r3:08x} r4=0x{r4:08x} r5=0x{r5:08x} r6=0x{r6:08x} sp=0x{sp:08x} lr=0x{lr:08x} object=0x{object:08x}:{object_bytes} indirect={indirect} filter_caller={filter_caller} filter_key=0x{filter_key:08x} filter_key_info=0x{filter_key_info:08x} filter_mask_a=0x{filter_mask_a:08x} filter_mask_b=0x{filter_mask_b:08x} filter_key_offset=0x{filter_key_offset:08x} up_allowed_a=0x{up_allowed_a:08x} up_allowed_b=0x{up_allowed_b:08x} filter_result=0x{filter_result:08x} rejected_up_override=0x{rejected_up_override:02x} keyboard_profile=0x{keyboard_profile:02x} pending_message=0x{pending_message:08x} pipe_gate={pipe_gate:02x} editor_gate={editor_gate:02x} editor_mode={editor_mode:02x} key_block_candidate={gate:02x} gate_ctx={gate_context} hkbd_config={hkbd_config} hkbd_active={hkbd_active} hkbd_key_record={hkbd_key_record} hkbd_record_type=0x{hkbd_record_type:04x} hkbd_record_index=0x{hkbd_record_index:04x} hkbd_record_key=0x{hkbd_record_key:04x} hkbd_config_index=0x{hkbd_config_index:04x} hkbd_config_value=0x{hkbd_config_value:02x} hkbd_mailbox_processes={hkbd_mailbox_processes} hkbd_mailbox_counts={hkbd_mailbox_counts} hkbd_mailbox_limits={hkbd_mailbox_limits} hkbd_mailbox_records={hkbd_mailbox_records} hkbd_process_directory={hkbd_process_directory} current_rtos_task={current_rtos_task} mcc_context={mcc_context} ui_command_gates={ui_command_gates} lk={lk_state}",
        menu_input_stage(address),
    );
    uc.get_data_mut().trace_menu_input_remaining -= 1;
}

pub(super) fn read_irq_nesting(uc: &Unicorn<'_, E830State>) -> u8 {
    // No heap allocation for a single diagnostic RAM byte.
    let mut byte = [u8::MAX];
    if uc.mem_read(RTOS_NESTING_DEPTH, &mut byte).is_ok() { byte[0] } else { u8::MAX }
}

pub(super) fn dispatch_table(uc: &Unicorn<'_, E830State>) -> Option<u32> {
    let node = uc.reg_read(RegisterARM::R5).ok()?;
    let bytes = diagnostic_ram_read(uc, node.checked_add(4)?, 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

// Interrupt controller (base 0x50302000), derived from the firmware's ARM IRQ
// vector handler at 0x20cb0cd0: it loads r4 = 0x50302000, reads the pending
// line number as a byte at the base (already multiplied by four), filters it
// against the pending bitmask at +0x1c, dispatches through a RAM table, then
// writes 0x80 to the end-of-interrupt register at +0x08 to acknowledge.
#[cold]
#[inline(never)]
pub(super) fn run_instruction_diagnostics(uc: &mut Unicorn<'_, E830State>, address: u64, total: u64) {
    if total >= uc.get_data().task_pc_log_after
        && uc.get_data().task_pc_log_remaining > 0
        && uc.get_data().task_pc_log_target.is_some()
    {
        let current = diagnostic_ram_read(uc, 0x3090_0194, 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .unwrap_or(0);
        let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0);
        if uc.get_data().task_pc_log_target == Some(current) && cpsr & 0x1f == 0x1f {
            eprintln!(
                "[e830-task-pc] task=0x{current:08x} pc=0x{address:08x} insns={total} cpsr=0x{cpsr:08x}"
            );
            uc.get_data_mut().task_pc_log_remaining -= 1;
        }
    }
    if uc.get_data().trace_tasks
        && total >= uc.get_data().trace_tasks_after
        && uc.get_data().trace_tasks_remaining > 0
        && uc.get_data().trace_tasks_pcs.contains(&address)
    {
        let word = |at: u64| -> Option<u32> {
            diagnostic_ram_read(uc, at, 4)
                .and_then(|bytes| bytes.try_into().ok()).map(u32::from_le_bytes)
        };
        let current = word(0x3090_0194).unwrap_or(0);
        let next = if address == 0x204b_01d8 {
            uc.reg_read(RegisterARM::R1).unwrap_or(0) as u32
        } else {
            0
        };
        let target = uc.get_data().trace_tasks_target;
        let send_target = if address == 0x2084_7a02 {
            uc.reg_read(RegisterARM::R5).unwrap_or(0) as u32
        } else {
            0
        };
        if target.is_none_or(|target| current == target || next == target || send_target == target) {
            let task = diagnostic_ram_read(uc, u64::from(current), 0x38).unwrap_or_default();
            let task_words: Vec<String> = task.chunks_exact(4)
                .map(|bytes| format!("{:08x}", u32::from_le_bytes(bytes.try_into().unwrap())))
                .collect();
            let registers = [RegisterARM::R0, RegisterARM::R1, RegisterARM::R4,
                RegisterARM::R5, RegisterARM::R6, RegisterARM::R7, RegisterARM::SP,
                RegisterARM::LR, RegisterARM::CPSR];
            let values: Vec<String> = registers.into_iter().map(|register|
                format!("{register:?}=0x{:08x}", uc.reg_read(register).unwrap_or(0))).collect();
            let object = uc.reg_read(if address == 0x2084_7d14 {
                RegisterARM::R4
            } else { RegisterARM::R7 }).unwrap_or(0);
            let object_words: Vec<String> = (0..4).map(|index|
                word(object.saturating_add(index * 4)).map(|value| format!("{value:08x}"))
                    .unwrap_or_else(|| "????????".into())).collect();
            let node = uc.reg_read(RegisterARM::R5).unwrap_or(0);
            let node_words: Vec<String> = (0..5).map(|index|
                word(node.saturating_add(index * 4)).map(|value| format!("{value:08x}"))
                    .unwrap_or_else(|| "????????".into())).collect();
            let argument = uc.reg_read(RegisterARM::R0).unwrap_or(0);
            let argument_words: Vec<String> = (0..3).map(|index|
                word(argument.saturating_add(index * 4)).map(|value| format!("{value:08x}"))
                    .unwrap_or_else(|| "????????".into())).collect();
            let saved_pc = |task: u32| -> Option<u32> {
                let saved_sp = word(u64::from(task) + 4)?;
                word(u64::from(saved_sp) + 56)
            };
            eprintln!(
                "[e830-task] pc=0x{address:08x} insns={total} current=0x{current:08x} next=0x{next:08x} saved_pc={:?} next_saved_pc={:?} task=[{}] object=[{}] node=[{}] arg=[{}] {}",
                saved_pc(current).map(|pc| format!("0x{pc:08x}")),
                saved_pc(next).map(|pc| format!("0x{pc:08x}")),
                task_words.join(","), object_words.join(","), node_words.join(","),
                argument_words.join(","), values.join(" ")
            );
            uc.get_data_mut().trace_tasks_remaining -= 1;
        }
    }

    trace_menu_input_stage(uc, address, total);

    if let Some(limit) = uc.get_data().trace_sp_below {
        let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0) as u32;
        let (last_sp, last_pc, after, remaining) = {
            let state = uc.get_data();
            (state.trace_sp_last, state.trace_sp_last_pc, state.trace_sp_after,
                state.trace_sp_remaining)
        };
        if total >= after && remaining > 0 && sp < limit && last_sp != Some(sp) {
            let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0) as u32;
            let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
            eprintln!(
                "[e830-sp] pc=0x{address:08x} prev_pc=0x{last_pc:08x} sp=0x{sp:08x} prev_sp=0x{:08x} lr=0x{lr:08x} cpsr=0x{cpsr:08x} insns={total}",
                last_sp.unwrap_or(0)
            );
            uc.get_data_mut().trace_sp_remaining -= 1;
        }
        let state = uc.get_data_mut();
        state.trace_sp_last = Some(sp);
        state.trace_sp_last_pc = address;
    }
    if !uc.get_data().watch_pcs.is_empty()
        && total >= uc.get_data().watch_after
        && uc.get_data().watch_remaining > 0
        && uc.get_data().watch_pcs.contains(&address)
    {
        let registers = [RegisterARM::R0, RegisterARM::R1, RegisterARM::R2, RegisterARM::R3,
            RegisterARM::R4, RegisterARM::R5, RegisterARM::R6, RegisterARM::R7,
            RegisterARM::SP, RegisterARM::LR, RegisterARM::CPSR];
        let values: Vec<(RegisterARM, u64)> = registers.into_iter().map(|register|
            (register, uc.reg_read(register).unwrap_or(0))).collect();
        let formatted: Vec<String> = values.iter().map(|(register, value)|
            format!("{register:?}=0x{value:08x}")).collect();
        let pointers: Vec<String> = values.iter().filter_map(|(register, value)| {
            if matches!(register, RegisterARM::SP | RegisterARM::LR | RegisterARM::CPSR) {
                return None;
            }
            diagnostic_watch_words(uc, *value, uc.get_data().watch_memory_bytes)
                .map(|words| format!("{register:?}->[{words}]"))
        }).collect();
        let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0);
        let stack = diagnostic_watch_words(uc, sp, uc.get_data().watch_memory_bytes)
            .unwrap_or_else(|| "unreadable".into());
        eprintln!(
            "[e830-watch] pc=0x{address:08x} insns={total} {} ptrs={} stack=[{stack}]",
            formatted.join(" "), pointers.join(" ")
        );
        uc.get_data_mut().watch_remaining -= 1;
    }
    if uc.get_data().trace_dsp && DSP_TRACE_PCS.contains(&address) {
        let should_log = {
            let state = uc.get_data();
            state.trace_dsp
                && total >= state.trace_dsp_after
                && state.trace_dsp_remaining > 0
        };
        if should_log {
            let mailbox = uc.mem_read_as_vec(DSP_MAILBOX_BASE, 64).unwrap_or_default();
            let word = |bytes: &[u8], offset: usize| -> u32 {
                bytes.get(offset..offset + 4)
                    .and_then(|slice| slice.try_into().ok())
                    .map(u32::from_le_bytes)
                    .unwrap_or(0)
            };
            let r0 = uc.reg_read(RegisterARM::R0).unwrap_or(0) as u32;
            let r1 = uc.reg_read(RegisterARM::R1).unwrap_or(0) as u32;
            let r4 = uc.reg_read(RegisterARM::R4).unwrap_or(0) as u32;
            let r6 = uc.reg_read(RegisterARM::R6).unwrap_or(0) as u32;
            let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0) as u32;
            let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0) as u32;
            let control = uc.get_data().dsp_boot.snapshot();
            let values = [
                control[0], control[1], control[2], control[3],
                word(&mailbox, 0), word(&mailbox, 0x10), word(&mailbox, 0x14),
                word(&mailbox, 0x18),
            ];
            let changed = uc.get_data().trace_dsp_last
                .map(|(last_pc, last_values)| last_pc != address || last_values != values)
                .unwrap_or(true);
            if changed {
                eprintln!(
                    "[e830-dsp] pc=0x{address:08x} ctl0=0x{:08x} ctl4=0x{:08x} ctl8=0x{:08x} ctlc=0x{:08x} mbx640=0x{:08x} mbx650=0x{:08x} mbx654=0x{:08x} mbx658=0x{:08x} r0=0x{r0:08x} r1=0x{r1:08x} r4=0x{r4:08x} r6=0x{r6:08x} sp=0x{sp:08x} lr=0x{lr:08x} insns={total}",
                    values[0], values[1], values[2], values[3], values[4], values[5],
                    values[6], values[7]
                );
                let state = uc.get_data_mut();
                state.trace_dsp_last = Some((address, values));
                state.trace_dsp_remaining -= 1;
            }
        }
    }

    if uc.get_data().trace_wake
        && ((WAKE_WAIT_TRACE_START..=WAKE_WAIT_TRACE_END).contains(&address)
            || WAKE_IRQ_TRACE_PCS.contains(&address))
    {
        let total = uc.get_data().instructions.saturating_add(uc.get_data().fast_forwarded_instructions);
        let after = uc.get_data().trace_wake_after;
        if total >= after && uc.get_data().trace_wake_remaining > 0 {
            let r0 = uc.reg_read(RegisterARM::R0).unwrap_or(0) as u32;
            let r1 = uc.reg_read(RegisterARM::R1).unwrap_or(0) as u32;
            let r4 = uc.reg_read(RegisterARM::R4).unwrap_or(0) as u32;
            let r5 = uc.reg_read(RegisterARM::R5).unwrap_or(0) as u32;
            let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0) as u32;
            let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0) as u32;
            let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
            uc.get_data_mut().trace_wake_line(format!(
                "pc=0x{address:08x} r0=0x{r0:08x} r1=0x{r1:08x} r4=0x{r4:08x} r5=0x{r5:08x} sp=0x{sp:08x} lr=0x{lr:08x} cpsr=0x{cpsr:08x} insns={total}"
            ));
        }
    }

    if uc.get_data().trace_timer && TIMER_TRACE_PCS.contains(&address) {
        let total = uc.get_data().instructions.saturating_add(uc.get_data().fast_forwarded_instructions);
        let after = uc.get_data().trace_timer_after;
        if total >= after && uc.get_data().trace_timer_remaining > 0 {
            let r0 = uc.reg_read(RegisterARM::R0).unwrap_or(0) as u32;
            let r1 = uc.reg_read(RegisterARM::R1).unwrap_or(0) as u32;
            let sp = uc.reg_read(RegisterARM::SP).unwrap_or(0) as u32;
            let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0) as u32;
            let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
            let state = uc.get_data();
            let mask = state.registers.get(&INTC_ENABLE_MASK_REGISTER).copied().unwrap_or(u32::MAX);
            let control = state.registers.get(&(SOC_TIMER + 8)).copied().unwrap_or(0);
            let period = state.registers.get(&(SOC_TIMER + 0x0c)).copied().unwrap_or(0);
            let match0 = state.registers.get(&(SOC_TIMER + 0x10)).copied().unwrap_or(0);
            let match1 = state.registers.get(&(SOC_TIMER + 0x14)).copied().unwrap_or(0);
            uc.get_data_mut().trace_timer_line(format!(
                "pc=0x{address:08x} r0=0x{r0:08x} r1=0x{r1:08x} sp=0x{sp:08x} lr=0x{lr:08x} cpsr=0x{cpsr:08x} mask=0x{mask:08x} ctrl=0x{control:08x} period=0x{period:08x} match0=0x{match0:08x} match1=0x{match1:08x} insns={total}"
            ));
        }
    }

    // Diagnostic: pin down whether/where early boot unmasks interrupts.
    {
        let trace = uc.get_data().trace_irqen;
        if trace {
            let (logged_i, logged_f) = {
                let s = uc.get_data();
                (s.irqen_logged_i, s.irqen_logged_f)
            };
            if !logged_i || !logged_f {
                let cpsr = uc.reg_read(RegisterARM::CPSR).unwrap_or(0) as u32;
                if !logged_i && cpsr & CPSR_IRQ_DISABLE == 0 {
                    let total = {
                        let s = uc.get_data();
                        s.instructions.saturating_add(s.fast_forwarded_instructions)
                    };
                    eprintln!(
                        "[e830-irqen] IRQ unmasked first at pc=0x{address:08x} cpsr=0x{cpsr:08x} after {total} insns"
                    );
                    uc.get_data_mut().irqen_logged_i = true;
                }
                if !logged_f && cpsr & (1 << 6) == 0 {
                    let total = {
                        let s = uc.get_data();
                        s.instructions.saturating_add(s.fast_forwarded_instructions)
                    };
                    eprintln!(
                        "[e830-irqen] FIQ unmasked first at pc=0x{address:08x} cpsr=0x{cpsr:08x} after {total} insns"
                    );
                    uc.get_data_mut().irqen_logged_f = true;
                }
            }
        }
    }
}
