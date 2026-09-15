use crate::random;

pub const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
pub const CODE_BITS: u32 = 60;
pub const CODE_LEN: usize = 10;

const BITS_PER_SYMBOL: u32 = 6;
const CODE_MASK: u64 = (1 << CODE_BITS) - 1;
const SYMBOL_MASK: u64 = (1 << BITS_PER_SYMBOL) - 1;

const _: () = assert!(CODE_LEN as u32 * BITS_PER_SYMBOL == CODE_BITS);

const fn alphabet_membership() -> [bool; 256] {
    let mut table = [false; 256];
    let mut index = 0;
    while index < ALPHABET.len() {
        table[ALPHABET[index] as usize] = true;
        index += 1;
    }
    table
}

static IN_ALPHABET: [bool; 256] = alphabet_membership();

fn encode(value: u64) -> String {
    let mut code = String::with_capacity(CODE_LEN);

    for symbol in 0..CODE_LEN as u32 {
        let shift = CODE_BITS - BITS_PER_SYMBOL * (symbol + 1);
        code.push(ALPHABET[((value >> shift) & SYMBOL_MASK) as usize] as char);
    }

    code
}

pub fn generate() -> String {
    encode(random::u64() & CODE_MASK)
}

pub fn is_valid(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|byte| IN_ALPHABET[byte as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_integer_maps_to_its_big_endian_six_bit_symbols() {
        let cases = [
            ("zero is the first symbol repeated", 0, "AAAAAAAAAA"),
            ("the low six bits land in the last symbol", 63, "AAAAAAAAA_"),
            ("the next-to-last symbol carries bits six to eleven", 63 << 6, "AAAAAAAA_A"),
            ("a full sixty-bit value", 0x123456789ABCDEF, "EjRWeJq83v"),
            ("every bit set is the last symbol repeated", CODE_MASK, "__________"),
        ];
        for (label, value, expected) in cases {
            assert_eq!(encode(value), expected, "{label}");
        }
    }

    #[test]
    fn code_mask_keeps_exactly_the_low_sixty_bits() {
        let cases = [
            ("bit fifty-nine is the highest bit the mask keeps", 1 << 59, true),
            ("bit sixty is the first bit the mask drops", 1 << 60, false),
        ];
        for (label, bit, expected) in cases {
            assert_eq!((bit & CODE_MASK) == bit, expected, "{label}");
        }
    }

    #[test]
    fn generated_codes_are_always_ten_in_alphabet_symbols() {
        for _ in 0..256 {
            let code = generate();
            assert!(is_valid(&code), "generated {code} is not a valid code");
        }
    }

    #[test]
    fn generated_codes_are_not_a_constant() {
        assert_ne!(generate(), generate(), "two generated codes never collide");
    }

    #[test]
    fn is_valid_rejects_anything_that_is_not_ten_alphabet_symbols() {
        let cases = [
            ("a generated code", "EjRWeJq83v", true),
            ("the all-zero code", "AAAAAAAAAA", true),
            ("both extra symbols", "----______", true),
            ("empty", "", false),
            ("one symbol short", "EjRWeJq83", false),
            ("one symbol long", "EjRWeJq83vv", false),
            ("a plus is not in the alphabet", "EjRWeJq83+", false),
            ("a slash is not in the alphabet", "EjRWeJq83/", false),
            ("a dot is not in the alphabet", "EjRWeJq83.", false),
            ("a path traversal attempt", "../../etcd", false),
            ("a multi-byte character counts its bytes", "EjRWeJq83é", false),
            ("a space", "EjRWeJq8 v", false),
        ];
        for (label, code, expected) in cases {
            assert_eq!(is_valid(code), expected, "{label}: {code:?}");
        }
    }
}
