fn main() -> i64:
    let x = 1 // inline // comment
    ~ { this comment style is stripped before parsing } ~
    x += 2 # inline hash comment
    x *= 3
    print("x =", x)
    println("url", "http://x//y#z")
    return x
