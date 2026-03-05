fn add1(x: i64) -> i64:
    return x + 1

fn main() -> i64:
    let encoded = ct_xor("secret", 23)
    let plain = xor_decode(encoded, 23)
    let h = ct_hash(plain)
    if plain == "secret" and h != 0:
        return add1(41)
    return 0
