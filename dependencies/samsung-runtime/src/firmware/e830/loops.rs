//! Byte-recognized, bounded execution shortcuts for the E830 firmware.
use super::*;

pub(super) const COUNTING_DELAY_LOOP: [u8; 10] = [0x01, 0x32, 0x01, 0x30, 0x88, 0x42, 0xfb, 0xd3, 0x70, 0x47];

pub(super) const LOOP_COUNTING: u8 = 1;
pub(super) const LOOP_NESTED: u8 = 2;
pub(super) const LOOP_MEMSET: u8 = 4;
pub(super) const LOOP_FADE: u8 = 8;

pub(super) fn collect_loop_kinds(sets: [HashSet<u64>; 4]) -> Vec<(u64, u8)> {
    let mut kinds = HashMap::new();
    for (index, pcs) in sets.into_iter().enumerate() {
        for pc in pcs {
            // Signatures can overlap (notably nested/fade delays). Preserve
            // every former membership result, including handler priority.
            *kinds.entry(pc).or_insert(0) |= 1 << index;
        }
    }
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_unstable_by_key(|(pc, _)| *pc);
    kinds
}

pub(super) fn loop_kinds_at(kinds: &[(u64, u8)], address: u64) -> u8 {
    kinds.binary_search_by_key(&address, |(pc, _)| *pc)
        .map(|index| kinds[index].1)
        .unwrap_or(0)
}

pub(super) fn find_counting_delay_loop_pcs(cla: &[u8]) -> HashSet<u64> {
    cla.windows(COUNTING_DELAY_LOOP.len())
        .enumerate()
        .filter_map(|(offset, bytes)| {
            (bytes == COUNTING_DELAY_LOOP.as_slice())
                .then_some(u64::from(FLASH_BASE) + offset as u64 + 2)
        })
        .collect()
}

pub(super) const NESTED_DELAY_LOOP: [u8; 16] = [
    0x10, 0x1c, 0xc0, 0x46, 0x01, 0x38, 0xfc, 0xd1, 0x01, 0x39, 0x00, 0x29, 0xf8, 0xd1, 0x04, 0xe0,
];

pub(super) fn find_nested_delay_loop_pcs(cla: &[u8]) -> HashSet<u64> {
    cla.windows(NESTED_DELAY_LOOP.len())
        .enumerate()
        .filter_map(|(offset, bytes)| {
            (bytes == NESTED_DELAY_LOOP.as_slice())
                .then_some(u64::from(FLASH_BASE) + offset as u64 + 2)
        })
        .collect()
}

pub(super) fn fast_forward_nested_delay(r0: u32, r1: u32, remaining_instructions: u64) -> Option<u64> {
    if r0 == 0 || r1 == 0 {
        return None;
    }
    let loop_instructions = u64::from(r0).checked_mul(3)?;
    (loop_instructions <= remaining_instructions).then_some(loop_instructions - 1)
}

pub(super) const ZERO_FILL_LOOP: [u8; 16] = [
    0x0c, 0x50, 0xa0, 0x28, 0x0c, 0x50, 0xa0, 0x28, 0x20, 0x10, 0x51, 0x22, 0xfb, 0xff, 0xff, 0x2a,
];

pub(super) const FADE_DELAY_LOOP: [u8; 6] = [0xc0, 0x46, 0x01, 0x38, 0xfc, 0xd1];

// Splash LCD-init table delay: `mov r8,r8; subs r0,#1; bne .-2`. The inner
// countdown (r0, reloaded from r2 each outer pass) is a pure software delay the
// firmware runs between LCD register writes while holding the boot logo. The PC
// registered is the `mov r8,r8` at the top of the inner loop; the fast-forward
// handler collapses the r0 countdown and resumes at the `subs r1,#1` six bytes
// on. Distinct byte shape from the earlier delay loops, so it needs its own
// detector.
pub(super) fn find_fade_delay_loop_pcs(cla: &[u8]) -> HashSet<u64> {
    cla.windows(FADE_DELAY_LOOP.len())
        .enumerate()
        .filter_map(|(offset, bytes)| {
            (bytes == FADE_DELAY_LOOP.as_slice()).then_some(u64::from(FLASH_BASE) + offset as u64)
        })
        .collect()
}

pub(super) fn find_memset_loop_pcs(cla: &[u8]) -> HashSet<u64> {
    cla.windows(ZERO_FILL_LOOP.len())
        .enumerate()
        .filter_map(|(offset, bytes)| {
            (bytes == ZERO_FILL_LOOP.as_slice()).then_some(u64::from(FLASH_BASE) + offset as u64)
        })
        .collect()
}

pub(super) struct ZeroFillPlan {
    pub(super) bytes: usize,
    pub(super) new_r0: u32,
    pub(super) new_r1: u32,
    pub(super) new_cpsr: u32,
    pub(super) skipped_instructions: u64,
}

pub(super) fn plan_zero_fill(
    r0: u32,
    r1: u32,
    cpsr: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    lr: u32,
) -> Option<ZeroFillPlan> {
    if r2 != 0 || r3 != 0 || r12 != 0 || lr != 0 {
        return None;
    }

    let carry_set = cpsr & (1 << 29) != 0;
    if !carry_set {
        return Some(ZeroFillPlan {
            bytes: 0,
            new_r0: r0,
            new_r1: r1,
            new_cpsr: cpsr,
            skipped_instructions: 4,
        });
    }

    let iterations = u64::from(r1 / 32) + 1;
    let byte_count = iterations.checked_mul(32)?;
    if byte_count > u64::from(RAM_SIZE) {
        return None;
    }
    let bytes = usize::try_from(byte_count).ok()?;
    let byte_count_u32 = u32::try_from(byte_count).ok()?;
    let new_r0 = r0.checked_add(byte_count_u32)?;
    let new_r1 = r1.wrapping_sub(byte_count_u32);
    let new_cpsr = (cpsr & !0xf000_0000) | 0x8000_0000;

    Some(ZeroFillPlan {
        bytes,
        new_r0,
        new_r1,
        new_cpsr,
        skipped_instructions: iterations.checked_mul(4)?,
    })
}

pub(super) fn fast_forward_counting_delay(
    r0: u32,
    r1: u32,
    r2: u32,
    max_skipped_instructions: u64,
) -> Option<(u32, u32, u64)> {
    if r0 >= r1 {
        return None;
    }
    let skipped_iterations = u64::from(r1 - r0 - 1).min(max_skipped_instructions / 4);
    if skipped_iterations == 0 {
        return None;
    }
    let skipped_iterations = u32::try_from(skipped_iterations).ok()?;
    Some((
        r0 + skipped_iterations,
        r2.wrapping_add(skipped_iterations),
        u64::from(skipped_iterations) * 4,
    ))
}
