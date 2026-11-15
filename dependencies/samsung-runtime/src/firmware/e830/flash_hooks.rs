//! Revision-specific interception of observed internal NOR command stores.
use super::*;

pub(super) fn handle_internal_flash_command_write(
    uc: &mut Unicorn<'_, E830State>,
    pc: u64,
) -> bool {
    let (address_register, value_register, expected_address, expected_value) = match pc {
        0x31f0_0034 | 0x2189_a528 | 0x0189_a528 | 0xeb89_a528 => {
            (RegisterARM::R3, RegisterARM::R1, 0x2000_0aaau32, 0x00aau16)
        }
        0x31f0_0038 | 0x2189_a52c | 0x0189_a52c | 0xeb89_a52c => {
            (RegisterARM::R1, RegisterARM::R2, 0x2000_0554u32, 0x0055u16)
        }
        0x31f0_003c | 0x2189_a530 | 0x0189_a530 | 0xeb89_a530 => {
            (RegisterARM::R1, RegisterARM::R0, 0x2008_2aaau32, 0x00c0u16)
        }
        _ => return false,
    };
    let target = uc.reg_read(address_register).unwrap_or(u64::MAX) as u32;
    let value = uc.reg_read(value_register).unwrap_or(u64::MAX) as u16;
    if target != expected_address || value != expected_value {
        return false;
    }
    if std::env::var_os("E830_TRACE_FLASH_COMMANDS").is_some() {
        eprintln!(
            "[e830-flash-command] pc=0x{pc:08x} address=0x{target:08x} value=0x{value:04x}"
        );
    }
    // These are AMD-style command cycles to the internal firmware flash, not
    // array programming.  Keep the immutable CLA bytes intact and consume the
    // store just as the external NOR command model consumes unlock commands.
    // Unicorn derives Thumb state from bit 0 on a PC write.  Resume at the
    // following halfword while keeping the copied RAM routine in Thumb mode.
    uc.reg_write(RegisterARM::PC, pc + 3).is_ok()
}
