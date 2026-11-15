// SPDX-License-Identifier: GPL-2.0-or-later
//! Manufacturer roadmap is separate from executable device profiles.
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub struct Manufacturer {
    pub name: &'static str,
    pub status: &'static str,
    pub detail: &'static str,
}

pub const MANUFACTURERS: &[Manufacturer] = &[
    Manufacturer { name: "Sony Ericsson", status: "Experimental", detail: "K750i R1DB001 CID49: original startup, standby and Settings with declared compatibility inputs." },
    Manufacturer { name: "Samsung", status: "Planned", detail: "No supported firmware profile yet." },
    Manufacturer { name: "Siemens", status: "Planned", detail: "No executable firmware profile yet." },
    Manufacturer { name: "BlackBerry", status: "Planned", detail: "No executable firmware profile yet." },
    Manufacturer { name: "LG", status: "Planned", detail: "No executable firmware profile yet." },
    Manufacturer { name: "Panasonic", status: "Planned", detail: "No executable firmware profile yet." },
];
