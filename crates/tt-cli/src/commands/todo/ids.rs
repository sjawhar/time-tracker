use std::collections::HashSet;

use uuid::Uuid;

const CROCKFORD: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

pub fn mint_todo_id(existing: &HashSet<String>) -> String {
    mint_todo_id_with(existing, || *Uuid::new_v4().as_bytes())
}

pub fn mint_todo_id_with(
    existing: &HashSet<String>,
    mut random_bytes: impl FnMut() -> [u8; 16],
) -> String {
    loop {
        let candidate = todo_id_from_bytes(random_bytes());
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
}

fn todo_id_from_bytes(bytes: [u8; 16]) -> String {
    let mut id = String::with_capacity(13);
    id.push_str("td_");
    for digit in 0..10 {
        let mut index = 0usize;
        for bit in 0..5 {
            let bit_offset = (digit * 5) + bit;
            let byte = usize::from(bytes[bit_offset / 8]);
            let shift = 7 - (bit_offset % 8);
            index = (index << 1) | ((byte >> shift) & 1);
        }
        id.push(char::from(CROCKFORD[index]));
    }
    id
}
