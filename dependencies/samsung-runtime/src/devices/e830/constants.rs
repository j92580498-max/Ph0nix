//! E830 board addresses, register semantics, IRQs and observed clock ratios.
use crate::core::memory::PAGE_SIZE;

pub(super) const LOW_VECTOR_BASE: u64 = 0x0000_0000;
pub(super) const LOW_VECTOR_SIZE: u64 = 0x0001_0000;
pub(super) const FLASH_ALIAS_BASE: u64 = 0x0001_0000;
pub(super) const FLASH_ALIAS_OFFSET: usize = 0x0001_0000;
pub(super) const E830_BOOT_ROM_MIRROR_BASE: u64 = 0xea00_0000;
pub(super) const DSP_AUX_RAM_BASE: u64 = 0x6010_0000;
pub(super) const DSP_AUX_RAM_SIZE: u64 = 0x0040_0000;
pub(super) const DSP_BOOT_MMIO_SIZE: u64 = PAGE_SIZE;
pub(super) const DSP_SHARED_RAM_BASE: u64 = 0x6000_0000;
pub(super) const DSP_SHARED_RAM_SIZE: u64 = 0x0001_0000;
pub(super) const ONENAND_BASE: u64 = 0x4800_0000;
pub(super) const ONENAND_SIZE: u64 = 0x0002_0000;
pub(super) const EXTERNAL_FLASH_WINDOW_BASE: u64 = 0x21c0_0000;
pub(super) const EXTERNAL_FLASH_WINDOW_SIZE: u64 = 0x0040_0000;
pub(super) const NSF_NOR_OFFSET: usize = 0x0002_0000;
pub(super) const SOC_BOOT_REGISTER_BASE: u64 = 0x5000_0000;
pub(super) const SOC_BOOT_REGISTER_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_MEMORY_CONTROL_BASE: u64 = 0x3200_0000;
pub(super) const SOC_MEMORY_CONTROL_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_CONFIG_REGISTER_BASE: u64 = 0x5030_0000;
pub(super) const SOC_CONFIG_REGISTER_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_SECONDARY_REGISTER_BASE: u64 = 0x5040_0000;
pub(super) const SOC_SECONDARY_REGISTER_SIZE: u64 = 0x0001_0000;
pub(super) const UART_BASES: [u32; 2] = [0x5040_3000, 0x5040_4000];
pub(super) const DMA_BASE: u32 = 0x5010_0000;
pub(super) const DMA_IRQ: u32 = 29;
pub(super) const DMA_UART0_TX_CONTROL: u32 = 0x8400_0000;
pub(super) const DMA_UART0_TX_CONFIG: u32 = 0x8841;
pub(super) const DMA_SIM_TX_CONFIG: u32 = 0xaa41;
pub(super) const DMA_SIM_RX_CONFIG: u32 = 0xb013;
pub(super) const DMA_BYTE_INSTRUCTIONS: u64 = 32;
pub(super) const I2C_BASE: u32 = 0x5040_5000;
pub(super) const I2C_IRQ: u32 = 28;
pub(super) const SERIAL_BASE: u32 = 0x5040_8000;
pub(super) const SECONDARY_TIMER_BASES: [u32; 2] = [0x5040_9000, 0x5040_a000];
pub(super) const SECONDARY_TIMER_IRQS: [u32; 2] = [18, 17];
pub(super) const SECONDARY_TIMER_CPU_CLOCK_RATIO: u64 = 8;
pub(super) const SECONDARY_TIMER_COMPARE_OFFSETS: [u32; 4] = [0x0c, 0x10, 0x14, 0x18];
pub(super) const SECONDARY_TIMER_CONTROL_BITS: [u32; 4] = [0x08, 0x20, 0x80, 0x200];
pub(super) const SECONDARY_TIMER_STATUS_BITS: [u32; 4] = [0x02, 0x04, 0x08, 0x10];
pub(super) const SYSTEM_TIMER_IRQ: u32 = 20;
pub(super) const SYSTEM_TIMER_MATCH_IRQ: u32 = 21;
pub(super) const SYSTEM_TIMER_CPU_CLOCK_RATIO: u64 = 8;
pub(super) const SYSTEM_TIMER_COMPARE_MASK: u16 = 0x3fff;
pub(super) const SYSTEM_TIMER_FRACTION_BITS: u32 = 2;
pub(super) const SYSTEM_TIMER_MATCH0_ENABLE: u32 = 0x200;
pub(super) const SYSTEM_TIMER_MATCH1_ENABLE: u32 = 0x400;
pub(super) const SYSTEM_TIMER_MATCH_CHANNELS: u32 =
    SYSTEM_TIMER_MATCH0_ENABLE | SYSTEM_TIMER_MATCH1_ENABLE;
pub(super) const SYSTEM_TIMER_IRQ20_ENABLE: u32 = 0x20;
pub(super) const SYSTEM_TIMER_IRQ21_ENABLE: u32 = 0x40;
pub(super) const SYSTEM_TIMER_IRQ20_STATUS: u32 = 0x80;
pub(super) const SYSTEM_TIMER_IRQ21_STATUS: u32 = 0x100;
pub(super) const SYSTEM_TIMER_STATUS_MASK: u32 =
    SYSTEM_TIMER_IRQ20_STATUS | SYSTEM_TIMER_IRQ21_STATUS;
pub(super) const SOC_POWER_CONTROL_BASE: u64 = 0x5010_0000;
pub(super) const SOC_POWER_CONTROL_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_REGISTER_BASE: u64 = 0x5050_0000;
pub(super) const SOC_REGISTER_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_EXTERNAL_CONTROL_BASE: u64 = 0x4400_0000;
pub(super) const SOC_EXTERNAL_CONTROL_SIZE: u64 = 0x0001_0000;
pub(super) const SOC_TIMER: u32 = 0x5050_1000;
pub(super) const DSP_CONTROL_BASE: u64 = 0x6010_0000;
pub(super) const DSP_SIGNAL_BASE: u32 = 0x5050_5000;
pub(super) const DSP_START_SIGNAL: u32 = DSP_SIGNAL_BASE + 0x10;
pub(super) const DSP_RUNTIME_SIGNAL: u32 = DSP_SIGNAL_BASE + 0x18;
pub(super) const DSP_RESULT_SIGNAL: u32 = DSP_SIGNAL_BASE + 0x1c;
pub(super) const DSP_SERIAL_BASE: u32 = 0x5030_1000;
pub(super) const DSP_SERIAL_IRQ: u32 = 9;
pub(super) const DSP_SERIAL_TX_ENABLE: u32 = 7;
pub(super) const DSP_SERIAL_TX_COMPLETE: u32 = 4;
pub(super) const DSP_SERIAL_STATUS_MASK: u32 = 0x3fc;
pub(super) const SOC_INIT_CONTROL_REGISTER: u32 = 0x5030_2008;
pub(super) const SOC_INIT_STATUS_REGISTER: u32 = 0x5030_2004;
pub(super) const SOC_BOOT_READY_INPUT_REGISTER: u32 = 0x5030_3050;
pub(super) const SOC_BOOT_READY_INPUT_MASK: u32 = 1 << 9;
pub(super) const GPIO_BASE: u32 = 0x5030_3000;
pub(super) const PMIC_GPIO_PIN: u32 = 1 << 3;
pub(super) const SOC_KEYPAD_INPUT_REGISTERS: [u32; 8] = [
    0x5050_4008,
    0x5050_400c,
    0x5050_4010,
    0x5050_4018,
    0x5050_401c,
    0x5050_4020,
    0x5050_4024,
    0x5050_4028,
];
pub(super) const RESET_CPSR: u64 = 0x0000_00d3;
pub(super) const MAX_RECENT_MMIO: usize = 64;
pub(super) const INTC_PENDING_NUMBER_REGISTER: u32 = 0x5030_2000;
pub(super) const INTC_PENDING_MASK_REGISTER: u32 = 0x5030_201c;
pub(super) const INTC_MASKED_PENDING_REGISTER: u32 = 0x5030_2014;
pub(super) const INTC_ENABLE_MASK_REGISTER: u32 = 0x5030_2010;
pub(super) const WAKE_MMIO_START: u32 = 0x5050_0000;
pub(super) const WAKE_MMIO_END: u32 = 0x5050_2018;
