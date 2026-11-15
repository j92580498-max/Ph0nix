pub const FLASH_BASE: u32 = 0x2000_0000;
pub const RAM_BASE: u32 = 0x3000_0000;
pub const RAM_SIZE: u32 = 0x0200_0000;
pub const E830_TFS_SOURCE_BASE: u64 = 0x1c00_0000;

// Capacities from the current firmware's boot region table; the mechanism lives in core.
pub(super) const E830_TCM_SIZE: u64 = 0x4000;
pub(super) const E830_DTCM_BASE: u64 = 0x5060_0000;
pub(super) const E830_TCMTR: u32 = (5 << 18) | (5 << 6);

pub fn validate_nand_image(path: &std::path::Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not open NAND {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err("Select a physical NAND file, not a directory.".into());
    }
    if !matches!(metadata.len(), 33_554_432 | 34_603_008) {
        return Err(format!(
            "Unsupported NAND size: {} bytes. Expected 33,554,432 (main only) or 34,603,008 (1024+32 per page). A .tfs package is not a NAND dump.",
            metadata.len()
        ));
    }
    Ok(())
}
