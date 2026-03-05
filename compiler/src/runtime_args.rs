pub fn script_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        return Vec::new();
    }

    if args[1].ends_with(".x") {
        return collect_script_args(&args[2..]);
    }
    if args[1] == "run" && args.len() >= 3 && args[2].ends_with(".x") {
        return collect_script_args(&args[3..]);
    }
    Vec::new()
}

pub fn argc() -> i64 {
    script_args().len() as i64
}

pub fn argv(index: i64) -> String {
    if index < 0 {
        return String::new();
    }
    script_args()
        .get(index as usize)
        .cloned()
        .unwrap_or_default()
}

fn collect_script_args(rest: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut passthrough = false;
    for arg in rest {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg == "--timings" {
            continue;
        }
        out.push(arg.clone());
    }
    out
}
