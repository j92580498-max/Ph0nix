//! Shared phone contact vocabulary. Device profiles supply electrical mappings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PhoneKey {
    One, Two, Three, Four, Five, Six, Seven, Eight, Nine, Zero, Star, Hash,
    SoftLeft, SoftRight, Call, End, Up, Down, Left, Right, Ok,
}

impl PhoneKey {
    pub const ALL: [Self; 21] = [Self::One, Self::Two, Self::Three, Self::Four,
        Self::Five, Self::Six, Self::Seven, Self::Eight, Self::Nine, Self::Zero,
        Self::Star, Self::Hash, Self::SoftLeft, Self::SoftRight, Self::Call,
        Self::End, Self::Up, Self::Down, Self::Left, Self::Right, Self::Ok];

    pub const fn bit(self) -> u32 { 1 << self as u8 }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "1" => Self::One, "2" => Self::Two, "3" => Self::Three,
            "4" => Self::Four, "5" => Self::Five, "6" => Self::Six,
            "7" => Self::Seven, "8" => Self::Eight, "9" => Self::Nine,
            "0" => Self::Zero, "star" | "*" => Self::Star, "hash" | "#" => Self::Hash,
            "soft-left" => Self::SoftLeft, "soft-right" => Self::SoftRight,
            "call" => Self::Call, "end" => Self::End, "up" => Self::Up,
            "down" => Self::Down, "left" => Self::Left, "right" => Self::Right,
            "ok" => Self::Ok, _ => return None,
        })
    }

}
