//! Addresses and signatures belonging to the E830XEGG1 firmware adapter.
pub(super) const E830_TFS_SOURCE_MIN_SIZE: u64 = 0x0001_0000;
pub(super) const E830_DSP_STARTUP_MAGIC: u32 = 0x7073_2b02;
pub(super) const SYSTEM_TIMER_BOOKKEEPING_START: u64 = 0x3001_a4f4;
pub(super) const SYSTEM_TIMER_BOOKKEEPING_END: u64 = 0x3001_a4f7;
pub(super) const RTOS_GLOBALS_TRACE_START: u64 = 0x3090_0160;
pub(super) const RTOS_GLOBALS_TRACE_END: u64 = 0x3090_01bf;
pub(super) const RTOS_NESTING_DEPTH: u64 = 0x3090_0169;
pub(super) const DSP_MAILBOX_BASE: u64 = 0x6000_0640;
pub(super) const E830_DSP_SPACE0_WORDS: usize = 0x6000;
pub(super) const E830_DSP_SPACE1_START: u16 = 0x7000;
pub(super) const E830_DSP_SPACE1_WORDS: usize = 0x1000;
pub(super) const E830_DSP_SPACE0_FNV64: u64 = 0xb25c_3385_c96d_670d;
pub(super) const E830_DSP_SPACE1_FNV64: u64 = 0xe39e_195e_7cdc_9922;
pub(super) const DSP_ASYNC_QUEUE: u32 = 0x30a2_c354;
pub(super) const DSP_ASYNC_CALLBACK: u32 = 0x215a_7cb9;
pub(super) const DSP_ASYNC_EVENT_HANDLER_PC: u64 = 0x204d_0388;
pub(super) const DSP_ASYNC_COMPLETE_DELAY: u64 = 4096;
pub(super) const DSP_TRACE_PCS: [u64; 10] = [
    0x2098_c180,
    0x2098_c188,
    0x2098_c214,
    0x2098_c220,
    0x2118_f212,
    0x2118_f21e,
    0x2118_f252,
    0x2118_f260,
    0x2118_f294,
    0x2118_f2a2,
];
pub(super) const RTOS_GLOBALS_TRACE_TARGETS: [(u64, u64); 4] = [
    (0x3090_0170, 0x3090_0173),
    (0x3090_018c, 0x3090_018f),
    (0x3090_01a0, 0x3090_01a3),
    (0x3090_01b8, 0x3090_01bb),
];
pub(super) const SOC_KEYPAD_SCAN_READ_PCS: [u32; 2] = [0x208d_aa30, 0x208d_aa5c];
pub(super) const INITIAL_STACK: u64 = 0x31f4_8f74;
pub(super) const KEYBOARD_INIT_TABLE: u32 = 0x20e8_2eac;
pub(super) const STANDBY_LOOK_TABLE: u32 = 0x2171_7f98;
pub(super) const STATE_TABLE_DISPATCH_PC: u64 = 0x2084_78f2;
pub(super) const MENU_INPUT_KEY_BLOCK_CANDIDATE: u64 = 0x3048_c264;
pub(super) const MENU_INPUT_TRACE_PCS: [u64; 67] = [
    0x215a_6236, // HKBD sends MOBI_HKBD_KEY_PRESSED_IND to EDP.
    0x20d7_5404, // EDP key-press pipe entry.
    0x20d7_547c, // EDP direct APPI_EDP_KEY_PRESSED_IND route.
    0x20d7_54da, // EDP alternate/editor route.
    0x20d7_6304, // EDP keyboard-event diffusion entry.
    0x2055_9318, // edp2_51NoDiffAppiMessages cross-build candidate.
    0x2055_933a, // The no-diff predicate returns true.
    0x2055_933e, // The no-diff predicate returns false.
    0x20d7_631e, // Diffusion has the no-diff predicate result in r0.
    0x20d7_632a, // Proven early free/return gate inside diffusion.
    0x20d7_6334, // No-diff special route when the key-block gate is clear.
    0x20d7_634e, // Diffusion continues toward the MMI recipient.
    0x2015_6b34, // a_lk_StandbyLook wildcard callback.
    0x2119_fe50, // lk1_1rout_fsm equivalent entered through the standby callback.
    0x2015_6e80, // lk2_set_keyboard equivalent; requested profile is in r0.
    0x2015_6eda, // First APPI_EDP_CHANGE_KBD_CONFIG_REQ send site.
    0x2015_6f72, // Per-profile APPI_EDP_CHANGE_KBD_CONFIG_REQ send site.
    0x2015_741a, // Late per-profile APPI_EDP_CHANGE_KBD_CONFIG_REQ send site.
    0x2015_708c, // Keyboard profile 4 branch entry.
    0x2015_70e6, // Keyboard profile 5 branch entry.
    0x215a_5fc0, // HKBD key-record lookup over the RAM table at 0x30031994.
    0x2158_2028, // EDP receives APPI_EDP_CHANGE_KBD_CONFIG_REQ.
    0x2158_2042, // EDP forwards MOBI_HKBD_CONFIG_REQ to HKBD.
    0x20d9_221c, // HKBD receives MOBI_HKBD_CONFIG_REQ and begins applying slots.
    0x20d9_225a, // HKBD has applied every configuration slot in the request.
    0x2014_0c1e, // HKBD compares one mailbox count before sending 0x2203.
    0x2014_0c3e, // HKBD decides whether all press-path mailbox checks passed.
    0x2014_0e9c, // HKBD release/short processing that can emit 0x2204.
    0x2014_0ea6, // HKBD compares the active key-record type.
    0x2014_0ebe, // HKBD compares the selected configuration-slot value.
    0x2014_0ec4, // Active key record/configuration is eligible for 0x2204.
    0x2014_0edc, // HKBD compares one mailbox count with its configured maximum.
    0x2014_0ef0, // HKBD decides whether all three mailbox checks passed.
    0x2014_0efe, // HKBD emits MOBI_HKBD_KEY_INFO_IND.
    0x2014_0f1a, // Active key record/configuration rejected the 0x2204 path.
    0x215a_6266, // Generic HKBD sender queues the completed 0x2204 payload to EDP.
    0x2055_79b8, // EDP consumes MOBI_HKBD_KEY_INFO_IND.
    0x2055_85e4, // EDP builds APPI_EDP_KEY_INFO_IND.
    0x2055_864c, // EDP forwards APPI_EDP_KEY_INFO_IND to MMI.
    0x211a_08b6, // MMI has loaded the APPI_EDP_KEY_PRESSED_IND payload.
    0x211a_27e6, // MMI direction-key branch inside KEY_PRESSED handling.
    0x211a_57c0, // lk2_33key_in_filter equivalent entry.
    0x211a_593a, // Up (0x14) filter branch.
    0x211a_599e, // Keyboard filter result in r0.
    0x200d_4cc4, // MCC_GETKEY_ACCEPT_REJECT installs masks/profile.
    0x2111_2a6c, // MCC_GETKEY_S installs mask A from its active descriptor.
    0x2153_656c, // MCC_GETKEY_INCALL installs mask A from its active descriptor.
    0x200d_4d94, // MCC_GETKEY_ACCEPT_REJECT_S calls the key filter.
    0x200d_4d9c, // MCC_GETKEY_ACCEPT_REJECT_S tests the result.
    0x200d_4dcc, // MCC_GETKEY_ACCEPT_REJECT_S rejected-key path.
    0x2111_2b84, // MCC_GETKEY_S calls the shared key filter.
    0x2111_2b8c, // MCC_GETKEY_S tests the filter result.
    0x2153_66a0, // MCC_GETKEY_INCALL_S calls the shared key filter.
    0x2153_66a8, // MCC_GETKEY_INCALL_S tests the filter result.
    0x2111_2b96, // MCC_GETKEY_S forwards an accepted key to UI dispatch.
    0x215b_8a30, // UI dispatch selects the Up (0x14) command.
    0x2159_dd48, // Common UI command dispatcher entry; command in r0.
    0x2159_dd60, // Common UI command dispatcher early return.
    0x2159_de34, // Common UI command dispatcher invokes the selected action.
    0x2159_defe, // Alternate common UI action invocation.
    0x214d_93e4, // Dispatch hook that invokes the real UI action (veneer to 0x2118c13c).
    0x2098_9e58, // HKBD clear: zeroes all 27 config slots (loop bound 0x1b).
    0x208e_51ac, // IRQ16 keypad handler entry; release source events arrive here.
    0x208a_74d0, // keypad callback post-release: physical-to-record resolve.
    0x208a_703c, // keypad callback post-release: sends resolved key to HKBD.
    0x2014_0c68, // HKBD press gate: reads active key record and config slot.
    0x2014_0d54, // HKBD release/short alternate handler (sends 0x2204/0x220e class).
];
pub(super) const RTOS_TASK_TRACE_PCS: [u64; 9] = [
    0x2084_7660, // receive: task group in r7, request in r5
    0x2084_7686, // event-mask wait has been stored in current task
    0x2084_76c4, // empty message queue wait
    0x2084_7724, // receive result: message in r7
    0x2084_7d14, // semaphore decrement: object in r4
    0x204b_01d8, // context switch: next task in r1
    0x2084_7a02, // message send: target task in r5, queue in r6
    0x2084_78f2, // state-table callback: table at [r5+4], entry in r4
    0x2084_7826, // direct callback: process slot in r7
];
pub(super) const IRQ_VECTOR_TARGET: u64 = 0x20cb_0cd0;
pub(super) const DEFAULT_TIMER_IRQ_INDEX: u32 = 29;
pub(super) const DEFAULT_TIMER_IRQ_PERIOD: u64 = 0;
pub(super) const WAKE_STATE_START: u64 = 0x304a_20d4;
pub(super) const WAKE_STATE_END: u64 = 0x304a_2104;
pub(super) const WAKE_STUB_START: u64 = 0x31f0_0000;
pub(super) const WAKE_STUB_END: u64 = 0x31f0_01ab;
pub(super) const WAKE_WAIT_TRACE_START: u64 = 0x2012_179c;
pub(super) const WAKE_WAIT_TRACE_END: u64 = 0x2012_17d6;
pub(super) const WAKE_IRQ_TRACE_PCS: [u64; 10] = [
    IRQ_VECTOR_TARGET,
    0x2012_1498,
    0x31f0_0140,
    0x208a_0fd8,
    0x2001_0bb2,
    0x2001_3d08,
    0x2001_0a40,
    0x2001_40cc,
    0x2001_07e8,
    0x20d7_29c0,
];
pub(super) const TIMER_TRACE_PCS: [u64; 7] = [
    0x2001_172c,
    0x2001_17d4,
    0x200e_8cd0,
    0x200e_8d08,
    0x2013_e2e8,
    0x2084_8732,
    IRQ_VECTOR_TARGET,
];
