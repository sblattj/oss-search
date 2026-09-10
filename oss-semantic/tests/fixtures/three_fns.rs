// Module-level comment, separated by a blank line below.

/// Fast identity-ish doubling.
#[inline]
pub fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 0,
        _ => n,
    }
}

/// Slow growth.
fn grow(x: i32) -> i32 {
    x + 1
}

fn shrink(x: i32) -> i32 {
    x - 1
}
