fn main() -> i64:
    let addr = 0
    if ptr_can_read(addr, 8):
        return ptr_read64(addr)
    print("pointer not readable:", addr)
    return 0
