fn add(a: i64, b: i64) -> i64:
    return a + b

fn choose(v: i64) -> i64:
    if v:
        is 1:
            return 10
        is 2:
            return 20
        else:
            return 0
    return 0

fn main() -> i64:
    return add(1, 2)

