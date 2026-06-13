use super::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{Map, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};

const ITERATIONS: usize = 200;
const MAX_DEPTH: u8 = 3;

fn random_string(rng: &mut StdRng, max_len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
    let len = rng.gen_range(1..=max_len);
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn random_value(rng: &mut StdRng, depth: u8) -> Value {
    if depth == 0 {
        return match rng.gen_range(0..4) {
            0 => Value::String(random_string(rng, 12)),
            1 => Value::Number(rng.gen_range(0..=9999).into()),
            2 => Value::Bool(rng.gen_bool(0.5)),
            _ => Value::Null,
        };
    }

    match rng.gen_range(0..4) {
        0 => Value::String(random_string(rng, 24)),
        1 => {
            let mut map = Map::new();
            let entries = rng.gen_range(0..=3);
            for _ in 0..entries {
                map.insert(random_string(rng, 10), random_value(rng, depth - 1));
            }
            Value::Object(map)
        }
        2 => {
            let items = rng.gen_range(0..=4);
            Value::Array((0..items).map(|_| random_value(rng, depth - 1)).collect())
        }
        _ => Value::Number(rng.gen_range(0..=9999).into()),
    }
}

#[test]
fn tool_call_id_from_params_fuzz_never_panics() {
    let mut rng = StdRng::seed_from_u64(0xC0D3_600D);
    for _ in 0..ITERATIONS {
        let value = random_value(&mut rng, MAX_DEPTH);
        let _ = catch_unwind(AssertUnwindSafe(|| tool_call_id_from_params(&value)));
    }
}
