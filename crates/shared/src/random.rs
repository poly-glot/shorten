pub fn bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("operating system entropy");
    bytes
}

pub fn u64() -> u64 {
    u64::from_le_bytes(bytes())
}
