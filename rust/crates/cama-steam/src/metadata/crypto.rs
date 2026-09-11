//! Rust port of the Apache-2.0 thedanill/dota_crypto decoder.
//! See dota_crypto_NOTICE and dota_crypto_LICENSE alongside this module.

mod table {
    include!("metadata_decrypt_table.rs");
}

fn prepare_keystream(key: u32) -> [u32; 48] {
    let a = (key ^ 0x0633_a998).to_be_bytes();
    let b = (key ^ 0x4e6c_32b9).to_be_bytes();
    let mut keys = [
        u16::from_le_bytes([a[0], a[1]]),
        u16::from_le_bytes([a[2], a[3]]),
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
    ];
    let mut stream = [0_u32; 48];
    for round in 0..16 {
        match round {
            1 | 2 | 3 | 7 | 8 => keys.rotate_left(1),
            4 | 5 | 10 | 12 | 14 => keys.rotate_right(1),
            6 | 9 | 11 | 13 | 15 => keys.rotate_left(2),
            _ => {}
        }
        for bit in 0..15 {
            let index = round * 3 + bit % 3;
            let nibble = keys
                .iter()
                .fold(0_u32, |value, key| (value << 1) | u32::from(key & 1));
            stream[index] = (stream[index] << 4) | nibble;
            for key in &mut keys {
                *key = (*key >> 1) | ((!*key & 1) << 15);
            }
        }
    }
    stream
}

fn xor_key(block: u32, stream: &[u32]) -> u32 {
    let scramble1 = ((block & 3) << 18) | ((block >> 24) << 10) | ((block >> 16) & 0x3ff);
    let scramble2 = ((block & (0x3ff << 8)) << 2) | (block & 0x3ff);
    let scrambled = stream[2] & (scramble1 ^ scramble2);
    let temp1 = scrambled ^ scramble2 ^ stream[1];
    let temp2 = scrambled ^ scramble1 ^ stream[0];
    table::DECRYPT_TABLE[((temp1 & 0x3ff) | 0xc00) as usize]
        | table::DECRYPT_TABLE[((temp2 & 0x3ff) | 0x400) as usize]
        | table::DECRYPT_TABLE[((temp1 >> 10) | 0x800) as usize]
        | table::DECRYPT_TABLE[(temp2 >> 10) as usize]
}

pub fn decrypt(data: &[u8], key: u32) -> Result<Vec<u8>, &'static str> {
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return Err("private metadata must contain complete eight-byte cipher blocks");
    }
    let stream = prepare_keystream(key);
    let mut output = Vec::with_capacity(data.len());
    for block in data.chunks_exact(8) {
        let mut left = u32::from_be_bytes(block[..4].try_into().expect("four-byte half block"));
        let mut right = u32::from_be_bytes(block[4..].try_into().expect("four-byte half block"));
        for round in (0..8).rev() {
            let end = (round + 1) * 6;
            left ^= xor_key(right, &stream[end - 3..end]);
            right ^= xor_key(left, &stream[end - 6..end - 3]);
        }
        output.extend_from_slice(&right.to_be_bytes());
        output.extend_from_slice(&left.to_be_bytes());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_partial_blocks_instead_of_silently_truncating() {
        assert!(decrypt(&[], 1).is_err());
        assert!(decrypt(&[0; 9], 1).is_err());
    }

    #[test]
    fn unsigned_high_bit_keys_produce_twenty_bit_round_keys() {
        for key in [0, 1, 0x8000_0000, u32::MAX] {
            assert!(
                prepare_keystream(key)
                    .iter()
                    .all(|round| *round <= 0x000f_ffff)
            );
            assert_eq!(decrypt(&[0; 8], key).unwrap().len(), 8);
        }
    }
}
