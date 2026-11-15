//! Validated memory mappings. Addresses and permissions come from profiles.
use crate::core::{
    cpu::{EngineProt as Prot, Unicorn},
    CoreError,
};
use serde::Serialize;

pub const PAGE_SIZE: u64 = 0x1000;

pub fn align_page(size: u64) -> u64 {
    (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Permissions {
    pub const RWX: Self = Self {
        read: true,
        write: true,
        execute: true,
    };
    pub const RW: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const RX: Self = Self {
        read: true,
        write: false,
        execute: true,
    };

    pub fn engine(self) -> Prot {
        let mut permissions = Prot::NONE;
        if self.read {
            permissions |= Prot::READ;
        }
        if self.write {
            permissions |= Prot::WRITE;
        }
        if self.execute {
            permissions |= Prot::EXEC;
        }
        permissions
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryRegion {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub permissions: Permissions,
}

impl MemoryRegion {
    pub fn contains(&self, address: u64, size: usize) -> bool {
        address >= self.address
            && address
                .checked_add(size as u64)
                .zip(self.address.checked_add(self.size))
                .is_some_and(|(end, limit)| end <= limit)
    }
}

/// Reject overlap, unaligned ranges and address overflow before mapping anything.
pub fn validate_regions(regions: &[MemoryRegion]) -> Result<(), String> {
    let mut ordered: Vec<_> = regions.iter().collect();
    ordered.sort_by_key(|region| region.address);
    let mut previous_end = 0;
    for region in ordered {
        let end = region
            .address
            .checked_add(region.size)
            .filter(|end| *end <= (1u64 << 32))
            .ok_or_else(|| format!("{} exceeds the 32-bit address space", region.name))?;
        if region.address % PAGE_SIZE != 0 || region.size == 0 || region.size % PAGE_SIZE != 0 {
            return Err(format!(
                "{} must have a nonempty, page-aligned range",
                region.name
            ));
        }
        if region.address < previous_end {
            return Err(format!("{} overlaps another region", region.name));
        }
        previous_end = end;
    }
    Ok(())
}

pub fn map_memory<D: 'static>(
    cpu: &mut Unicorn<'static, D>,
    address: u64,
    size: u64,
    permissions: Prot,
    operation: &'static str,
) -> Result<(), CoreError> {
    cpu.mem_map(address, size, permissions)
        .map_err(|error| CoreError::Unicorn {
            operation,
            error: format!("{error:?}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn region(address: u64, size: u64) -> MemoryRegion {
        MemoryRegion {
            name: "test".into(),
            address,
            size,
            permissions: Permissions::RW,
        }
    }
    #[test]
    fn invalid_maps_are_rejected_before_partial_installation() {
        assert!(validate_regions(&[region(0x1000, 0x1000), region(0x2000, 0x2000)]).is_ok());
        for regions in [
            vec![region(1, 0x1000)],
            vec![region(0x1000, 0)],
            vec![region(0x1000, 0x2000), region(0x2000, 0x1000)],
            vec![region(0xffff_f000, 0x2000)],
        ] {
            assert!(validate_regions(&regions).is_err());
        }
        assert!(!region(u64::MAX - 4, 16).contains(u64::MAX - 2, 8));
    }
}
