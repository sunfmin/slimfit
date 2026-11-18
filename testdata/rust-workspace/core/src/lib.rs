// `double` has no test in this package: only the `app` package's test
// reaches it. `name` is tested here, so this package has a test to run.
pub fn double(x: i64) -> i64 {
    x * 2
}

pub fn name() -> &'static str {
    "core"
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        assert_eq!(super::name(), "core");
    }
}
