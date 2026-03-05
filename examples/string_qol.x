fn main() -> i64:
    let s = "  alpha,beta,gamma  "
    let mid = s.trim().split(",", 1)
    let ix = mid.find("beta")
    let merged = "alpha".join("gamma", "-")
    println("mid:", mid, "ix:", ix, "merged:", merged)
    return 0
