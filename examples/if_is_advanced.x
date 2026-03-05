fn main() -> i64:
    let s = "hello-world"
    if s
        is starts_with "hello":
            return 10
        is contains "orl":
            return 20
        is ends_with "zzz":
            return 30
    return 0

