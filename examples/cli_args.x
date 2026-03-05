fn main() -> i64:
    print("argc:", argc())
    let first = argv(0)
    if len(first):
        print("argv0:", first)
    else:
        print("argv0: <none>")
    print("sleeping 10ms...")
    sleep_ms(10)
    print("done.")
    return 0
