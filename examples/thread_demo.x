fn worker(tag: str) -> i64:
    print("worker:", tag, "start")
    sleep_ms(10)
    print("worker:", tag, "done")
    return 0

fn main() -> i64:
    worker("A") -> thread(3).wait()

    let n = 2
    while n: -> thread(2).wait()
        sleep_ms(5)
        n -= 1

    print("all threaded work finished")
    return 0
