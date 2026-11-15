//! MMIO wiring mechanism. Register addresses and device behavior stay in profiles.
use crate::core::cpu::Unicorn;
use unicorn_engine::unicorn_const::uc_error;

pub fn map_bank<D: 'static, R, W>(
    cpu: &mut Unicorn<'static, D>,
    base: u64,
    size: u64,
    read: Option<R>,
    write: Option<W>,
) -> Result<(), uc_error>
where
    R: FnMut(&mut Unicorn<'_, D>, u64, usize) -> u64 + 'static,
    W: FnMut(&mut Unicorn<'_, D>, u64, usize, u64) + 'static,
{
    cpu.mmio_map(base, size, read, write)
}
