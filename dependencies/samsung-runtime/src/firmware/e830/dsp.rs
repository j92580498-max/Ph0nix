//! DSP upload protocol, recognized images and opt-in firmware HLE.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DspBootParser {
    Command,
    Running,
    RuntimeReadArguments {
        memory_space: u8,
        address: Option<u16>,
    },
    Arguments {
        command: u16,
        address: Option<u16>,
    },
    Upload {
        memory_space: u8,
        address: u16,
        remaining: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DspEvent {
    StartupReady,
    RuntimeReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DspHlePhase {
    Disabled,
    AwaitingStart,
    E830Running,
    E830RuntimeReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DspQueueHlePhase {
    Idle,
    CompletePending { deadline: u64 },
    CompleteSignaled,
}

#[derive(Debug)]
pub(super) struct DspBootInterface {
    pub(super) control: u32,
    pub(super) last_tx: u16,
    pub(super) auxiliary: u32,
    pub(super) parser: DspBootParser,
    pub(super) rx_fifo: VecDeque<u16>,
    pub(super) memories: [HashMap<u16, u16>; 2],
    pub(super) hle_phase: DspHlePhase,
    pub(super) runtime_completion_pending: bool,
}

impl Default for DspBootInterface {
    fn default() -> Self {
        Self::new(false)
    }
}

impl DspBootInterface {
    pub(super) fn new(hle_enabled: bool) -> Self {
        Self {
            control: 0,
            last_tx: 0,
            auxiliary: 0,
            parser: DspBootParser::Command,
            rx_fifo: VecDeque::new(),
            memories: Default::default(),
            hle_phase: if hle_enabled {
                DspHlePhase::AwaitingStart
            } else {
                DspHlePhase::Disabled
            },
            runtime_completion_pending: false,
        }
    }

    pub(super) const TX_READY: u32 = 1 << 0;
    pub(super) const RX_READY: u32 = 1 << 1;

    pub(super) fn status(&self) -> u32 {
        (self.control & !(Self::TX_READY | Self::RX_READY))
            | Self::TX_READY
            | if self.rx_fifo.is_empty() { 0 } else { Self::RX_READY }
    }

    pub(super) fn snapshot(&self) -> [u32; 4] {
        [
            self.status(),
            u32::from(self.rx_fifo.front().copied().unwrap_or(0)),
            u32::from(self.last_tx),
            self.auxiliary,
        ]
    }

    pub(super) fn memory_snapshot(&self, memory_space: usize) -> Option<Vec<u8>> {
        let memory = self.memories.get(memory_space)?;
        let words = match memory_space {
            0 => 0x1_0000,
            1 => 0x1_0000,
            _ => return None,
        };
        let mut bytes = Vec::with_capacity(words * 2);
        for address in 0..words {
            bytes.extend_from_slice(&memory.get(&(address as u16)).copied().unwrap_or(0).to_le_bytes());
        }
        Some(bytes)
    }

    pub(super) fn read(&mut self, address: u32, size: usize) -> u64 {
        let offset = address.wrapping_sub(DSP_CONTROL_BASE as u32);
        let aligned = offset & !3;
        let raw = match aligned {
            0 => self.status(),
            4 => u32::from(self.rx_fifo.pop_front().unwrap_or(0)),
            8 => u32::from(self.last_tx),
            12 => self.auxiliary,
            _ => 0,
        };
        let shift = (offset & 3) * 8;
        u64::from((raw >> shift) & width_mask(size))
    }

    pub(super) fn write(&mut self, address: u32, size: usize, value: u64) -> Option<DspEvent> {
        let offset = address.wrapping_sub(DSP_CONTROL_BASE as u32);
        let aligned = offset & !3;
        if aligned == 8 {
            self.last_tx = value as u16;
            return self.transmit(self.last_tx);
        }

        let shift = (offset & 3) * 8;
        let value_mask = width_mask(size);
        let mask = value_mask.wrapping_shl(shift);
        let update = |register: &mut u32| {
            *register = (*register & !mask) | (((value as u32).wrapping_shl(shift)) & mask);
        };
        match aligned {
            0 => update(&mut self.control),
            12 => update(&mut self.auxiliary),
            _ => {}
        }
        None
    }

    pub(super) fn transmit(&mut self, word: u16) -> Option<DspEvent> {
        if std::env::var_os("E830_TRACE_DSP_TX").is_some()
            && matches!(
                self.hle_phase,
                DspHlePhase::E830Running | DspHlePhase::E830RuntimeReady
            )
        {
            eprintln!(
                "[e830-dsp-tx] parser={:?} phase={:?} word=0x{word:04x}",
                self.parser, self.hle_phase
            );
        }
        match self.parser {
            DspBootParser::Command => match word {
                0x000b => self.rx_fifo.push_back(0x0305),
                0x0001 | 0x0002 | 0x0005 | 0x0006 | 0x0003 => {
                    self.parser = DspBootParser::Arguments {
                        command: word,
                        address: None,
                    };
                }
                // Runtime DSP commands are intentionally left for the runtime
                // mailbox model. TX remains ready, but boot ROM behavior is not
                // invented for commands not established by the loader.
                _ => {}
            },
            DspBootParser::Arguments { command: 0x0003, .. } => {
                // Start execution takes one address word and has no boot-ROM
                // response. DSP execution itself is not implemented here.
                self.parser = DspBootParser::Running;
                if self.hle_phase == DspHlePhase::AwaitingStart
                    && word == 0
                    && self.matches_e830_image()
                {
                    self.hle_phase = DspHlePhase::E830Running;
                    return Some(DspEvent::StartupReady);
                }
                self.hle_phase = DspHlePhase::Disabled;
            }
            DspBootParser::Running => {
                if matches!(self.hle_phase, DspHlePhase::E830Running | DspHlePhase::E830RuntimeReady) {
                    match word {
                        // The ARM continues using the boot interface's two
                        // established read commands after DSP start to verify
                        // and copy overlays. Return only bytes uploaded earlier.
                        0x0005 | 0x0006 => {
                            self.parser = DspBootParser::RuntimeReadArguments {
                                memory_space: if word == 0x0005 { 0 } else { 1 },
                                address: None,
                            };
                        }
                        0x1001 => {
                            // This command has two observed phases: the DSP
                            // accepts it, then raises the same signal again
                            // after the ARM acknowledges the first event.
                            self.runtime_completion_pending = true;
                            self.hle_phase = DspHlePhase::E830RuntimeReady;
                            return Some(DspEvent::RuntimeReady);
                        }
                        0x1002 | 0x2001 => {
                            // Retail call sites send these commands and wait
                            // for one DSP runtime event before returning.
                            // Without RD16024 execution, model only that
                            // observed completion notification.
                            self.hle_phase = DspHlePhase::E830RuntimeReady;
                            return Some(DspEvent::RuntimeReady);
                        }
                        0x4001 => {
                            self.hle_phase = DspHlePhase::E830RuntimeReady;
                            return Some(DspEvent::RuntimeReady);
                        }
                        _ => {}
                    }
                }
            }
            DspBootParser::RuntimeReadArguments { memory_space, address: None } => {
                self.parser = DspBootParser::RuntimeReadArguments {
                    memory_space,
                    address: Some(word),
                };
            }
            DspBootParser::RuntimeReadArguments { memory_space, address: Some(address) } => {
                for index in 0..word {
                    let location = address.wrapping_add(index);
                    self.rx_fifo.push_back(
                        self.memories[memory_space as usize]
                            .get(&location)
                            .copied()
                            .unwrap_or(0),
                    );
                }
                self.parser = DspBootParser::Running;
            }
            DspBootParser::Arguments { command, address: None } => {
                self.parser = DspBootParser::Arguments {
                    command,
                    address: Some(word),
                };
            }
            DspBootParser::Arguments {
                command,
                address: Some(address),
            } => {
                let memory_space = match command {
                    0x0001 | 0x0005 => 0,
                    0x0002 | 0x0006 => 1,
                    _ => {
                        self.parser = DspBootParser::Command;
                        return None;
                    }
                };
                if matches!(command, 0x0001 | 0x0002) {
                    self.parser = if word == 0 {
                        DspBootParser::Command
                    } else {
                        DspBootParser::Upload {
                            memory_space,
                            address,
                            remaining: word,
                        }
                    };
                } else {
                    for index in 0..word {
                        let location = address.wrapping_add(index);
                        self.rx_fifo.push_back(
                            self.memories[memory_space as usize]
                                .get(&location)
                                .copied()
                                .unwrap_or(0),
                        );
                    }
                    self.parser = DspBootParser::Command;
                }
            }
            DspBootParser::Upload {
                memory_space,
                address,
                remaining,
            } => {
                self.memories[memory_space as usize].insert(address, word);
                self.parser = if remaining == 1 {
                    DspBootParser::Command
                } else {
                    DspBootParser::Upload {
                        memory_space,
                        address: address.wrapping_add(1),
                        remaining: remaining - 1,
                    }
                };
            }
        }
        None
    }

    pub(super) fn matches_e830_image(&self) -> bool {
        self.memories[0].len() == E830_DSP_SPACE0_WORDS
            && self.memories[1].len() == E830_DSP_SPACE1_WORDS
            && self.memory_hash(0, 0, E830_DSP_SPACE0_WORDS) == Some(E830_DSP_SPACE0_FNV64)
            && self.memory_hash(1, E830_DSP_SPACE1_START, E830_DSP_SPACE1_WORDS)
                == Some(E830_DSP_SPACE1_FNV64)
    }

    pub(super) fn memory_hash(&self, space: usize, start: u16, count: usize) -> Option<u64> {
        let memory = self.memories.get(space)?;
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for offset in 0..count {
            let address = usize::from(start).checked_add(offset)?;
            let word = memory.get(&(u16::try_from(address).ok()?))?;
            for byte in word.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        Some(hash)
    }

    pub(super) fn runtime_signal_acknowledged(&mut self) -> bool {
        std::mem::take(&mut self.runtime_completion_pending)
    }

    pub(super) fn runtime_hle_active(&self) -> bool {
        matches!(
            self.hle_phase,
            DspHlePhase::E830Running | DspHlePhase::E830RuntimeReady
        )
    }

}
