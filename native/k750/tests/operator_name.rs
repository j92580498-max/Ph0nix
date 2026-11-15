//! Execute the published guest helper to check its ARM ABI and text ownership.
use unicorn_engine::{
    unicorn_const::{Arch, Mode, Prot},
    RegisterARM, Unicorn,
};

const DISPLAY: u64 = 0x4c001000;
const LAYOUT: u64 = 0x4c002000;
const TABLE: u64 = 0x4c003000;
const DESCRIPTOR: u64 = 0x4c004000;
const STRING: u64 = 0x4c005000;
const STACK: u64 = 0x4c0ff000;
const EMPTY: u32 = 0x6fffffff;
const NAME_ID: u32 = 0x80000041;

#[derive(Default)]
struct Calls {
    allocations: usize,
    updates: Vec<(u32, u32)>,
    fail_allocation: bool,
}

fn word(uc: &Unicorn<Calls>, address: u64) -> u32 {
    u32::from_le_bytes(uc.mem_read_as_vec(address, 4).unwrap().try_into().unwrap())
}
fn write_word(uc: &mut Unicorn<Calls>, address: u64, value: u32) {
    uc.mem_write(address, &value.to_le_bytes()).unwrap();
}
fn code() -> Vec<u8> {
    include_str!("../../../qemu/hw/arm/db2010-operator-name.inc")
        .split_once('{')
        .unwrap()
        .1
        .split('}')
        .next()
        .unwrap()
        .split(',')
        .filter_map(|part| part.trim().strip_prefix("0x"))
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}
fn setup(name: u32, status: u32, existing_text: Option<&str>) -> Unicorn<'static, Calls> {
    let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::THUMB, Calls::default()).unwrap();
    uc.mem_map(0x44000000, 0x01000000, Prot::ALL).unwrap();
    uc.mem_map(0x4c000000, 0x00100000, Prot::ALL).unwrap();
    uc.mem_write(0x44018000, &code()).unwrap();
    write_word(&mut uc, DISPLAY + 0xcc, LAYOUT as u32);
    write_word(&mut uc, LAYOUT + 0x78, TABLE as u32);
    for index in 0..16 {
        write_word(&mut uc, TABLE + index * 12 + 4, EMPTY);
    }
    write_word(&mut uc, TABLE + 16, name);
    write_word(&mut uc, TABLE + 28, status);
    write_word(&mut uc, TABLE + 11 * 12 + 4, 0x88000007); // Clock must stay intact.
    write_word(&mut uc, TABLE + 12 * 12 + 4, 0x88000008);
    if let Some(text) = existing_text {
        write_word(&mut uc, DESCRIPTOR, name);
        write_word(&mut uc, DESCRIPTOR + 4, STRING as u32);
        uc.mem_write(
            DESCRIPTOR + 8,
            &(text.encode_utf16().count() as u16).to_le_bytes(),
        )
        .unwrap();
        let mut bytes: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        bytes.extend_from_slice(&[0, 0]);
        uc.mem_write(STRING, &bytes).unwrap();
    }
    for address in [0x44eeb4ac, 0x44eebddc, 0x44e7fc84] {
        uc.mem_write(address, &[0x70, 0x47]).unwrap(); // Mock callee: bx lr.
    }
    uc.add_code_hook(1, 0, |uc, address, _| {
        match address {
            0x44eeb4ac => {
                uc.reg_write(RegisterARM::R0, DESCRIPTOR).unwrap();
            }
            0x44eebddc => {
                assert_eq!(uc.reg_read(RegisterARM::R1).unwrap(), 0); // UCS2
                assert_eq!(uc.reg_read(RegisterARM::R2).unwrap(), 6);
                let ptr = uc.reg_read(RegisterARM::R0).unwrap();
                let bytes = uc.mem_read_as_vec(ptr, 14).unwrap();
                let expected: Vec<_> = "ph0nix\0"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                assert_eq!(bytes, expected);
                uc.get_data_mut().allocations += 1;
                if uc.get_data().fail_allocation {
                    uc.reg_write(RegisterARM::R0, EMPTY.into()).unwrap();
                } else {
                    write_word(uc, DESCRIPTOR, NAME_ID);
                    write_word(uc, DESCRIPTOR + 4, ptr as u32);
                    uc.mem_write(DESCRIPTOR + 8, &6u16.to_le_bytes()).unwrap();
                    uc.reg_write(RegisterARM::R0, NAME_ID.into()).unwrap();
                }
            }
            0x44e7fc84 => {
                assert_eq!(uc.reg_read(RegisterARM::R0).unwrap(), LAYOUT);
                assert_eq!(uc.reg_read(RegisterARM::R3).unwrap(), 0);
                let item = uc.reg_read(RegisterARM::R1).unwrap() as u32;
                let text = uc.reg_read(RegisterARM::R2).unwrap() as u32;
                uc.get_data_mut().updates.push((item, text));
                write_word(uc, TABLE + item as u64 * 12 + 4, text);
            }
            0x44e98c00 => {
                uc.emu_stop().unwrap();
            }
            _ => {}
        }
    })
    .unwrap();
    uc
}
fn redraw(uc: &mut Unicorn<Calls>, display: u64) {
    for (reg, value) in [
        (RegisterARM::R0, display),
        (RegisterARM::R1, 0x1234),
        (RegisterARM::R2, 0x5678),
        (RegisterARM::R4, 0x1111),
        (RegisterARM::R5, 0x2222),
        (RegisterARM::R6, 0x3333),
        (RegisterARM::R7, 0x4444),
        (RegisterARM::SP, STACK),
        (RegisterARM::LR, 0x44019001),
    ] {
        uc.reg_write(reg, value).unwrap();
    }
    uc.emu_start(0x44018001, 0x44e98c02, 0, 1000).unwrap();
    assert_eq!(uc.reg_read(RegisterARM::R0).unwrap(), display);
    assert_eq!(uc.reg_read(RegisterARM::R1).unwrap(), 0x1234);
    assert_eq!(uc.reg_read(RegisterARM::R2).unwrap(), 0x5678);
    assert_eq!(uc.reg_read(RegisterARM::R4).unwrap(), display);
    assert_eq!(uc.reg_read(RegisterARM::R5).unwrap(), 0x2222);
    assert_eq!(uc.reg_read(RegisterARM::R6).unwrap(), 0x5678);
    assert_eq!(uc.reg_read(RegisterARM::R7).unwrap(), 0x4444);
    assert_eq!(uc.reg_read(RegisterARM::LR).unwrap(), 0x44019001);
    assert_eq!(uc.reg_read(RegisterARM::SP).unwrap(), STACK - 20);
    assert_eq!(
        uc.mem_read_as_vec(STACK - 20, 20).unwrap(),
        [0x1234u32, 0x1111, 0x2222, 0x3333, 0x44019001]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        (uc.reg_read(RegisterARM::CPSR).unwrap() >> 30) & 1,
        u64::from(display == 0)
    );
    assert_eq!(word(uc, TABLE + 11 * 12 + 4), 0x88000007);
    assert_eq!(word(uc, TABLE + 12 * 12 + 4), 0x88000008);
}

#[test]
fn blank_and_inactive_labels_use_one_centered_operator_field() {
    for status in [EMPTY, 0x1083, 0x1086, 0x70001080] {
        let mut uc = setup(EMPTY, status, None);
        redraw(&mut uc, DISPLAY);
        assert_eq!(word(&uc, TABLE + 16), NAME_ID);
        assert_eq!(word(&uc, TABLE + 28), EMPTY);
        assert_eq!(uc.get_data().allocations, 1);
        assert!(uc
            .get_data()
            .updates
            .iter()
            .all(|(item, _)| [1, 2].contains(item)));
        for _ in 0..20 {
            redraw(&mut uc, DISPLAY);
        }
        assert_eq!(uc.get_data().allocations, 1); // No allocation per subsequent frame.
    }
}
#[test]
fn both_unicode_ownership_modes_reuse_the_exact_name() {
    for name in [0x80000041, 0x88000041] {
        let mut uc = setup(name, 0x1083, Some("ph0nix"));
        redraw(&mut uc, DISPLAY);
        assert_eq!(uc.get_data().allocations, 0);
        assert_eq!(word(&uc, TABLE + 16), name);
        assert_eq!(uc.get_data().updates, vec![(2, EMPTY)]);
    }
}
#[test]
fn another_name_and_letter_o_are_replaced() {
    for name in ["phOnix", "phonix", "PH0NIX", "Other!"] {
        let mut uc = setup(0x88000041, EMPTY, Some(name));
        redraw(&mut uc, DISPLAY);
        assert_eq!(uc.get_data().allocations, 1);
        assert_eq!(word(&uc, TABLE + 16), NAME_ID);
    }
}
#[test]
fn allocation_failure_keeps_the_original_renderer_and_can_retry() {
    let mut uc = setup(EMPTY, 0x1083, None);
    uc.get_data_mut().fail_allocation = true;
    redraw(&mut uc, DISPLAY);
    assert!(uc.get_data().updates.is_empty());
    assert_eq!(word(&uc, TABLE + 28), 0x1083);
    uc.get_data_mut().fail_allocation = false;
    redraw(&mut uc, DISPLAY);
    assert_eq!(word(&uc, TABLE + 16), NAME_ID);
}
#[test]
fn absent_layout_and_null_display_preserve_the_original_abi() {
    let mut uc = setup(EMPTY, 0x1083, None);
    write_word(&mut uc, DISPLAY + 0xcc, 0);
    redraw(&mut uc, DISPLAY);
    redraw(&mut uc, 0);
    assert_eq!(uc.get_data().allocations, 0);
    assert!(uc.get_data().updates.is_empty());
}
