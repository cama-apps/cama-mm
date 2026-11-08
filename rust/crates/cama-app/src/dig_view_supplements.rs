//! Shared Dig view presentation helpers.

#[must_use]
pub const fn layer_color(layer: &str) -> u32 {
    match layer.as_bytes() {
        b"Dirt" => 0x8b_45_13,
        b"Stone" => 0x80_80_80,
        b"Crystal" => 0x00_ce_d1,
        b"Magma" => 0xff_45_00,
        b"Abyss" => 0x2f_00_47,
        b"Fungal Depths" => 0x7c_fc_00,
        b"Frozen Core" => 0x87_ce_eb,
        b"The Hollow" => 0x0d_0d_0d,
        _ => 0x8b_45_13,
    }
}
