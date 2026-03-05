fn main() -> i64:
    let total = 0
    for i in 0..12 step 3:
        total += i

    let back = 0
    for j in 12..0 step -5:
        back += j

    print("total", total, "back", back)
    println()
    return total + back

