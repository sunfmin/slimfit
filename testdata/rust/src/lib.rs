pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn unused(x: i64) -> i64 {
    let doubled = x * 2;
    let shifted = doubled + 7;
    shifted / 3
}

pub fn is_adult(age: u32) -> bool {
    age >= 18
}

pub fn negate(x: i64) -> i64 {
    -x
}

pub fn triple(x: i64) -> i64 {
    x * 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn adds_again() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn adult() {
        assert!(is_adult(30));
    }

    #[test]
    fn negates() {
        assert_eq!(negate(5), -5);
    }

    #[test]
    fn triples() {
        assert_eq!(triple(4), 12);
    }

    #[test]
    fn slow() {
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert_eq!(add(1, 1), 2);
    }
}
