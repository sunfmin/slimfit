//! A failure as slimfit reports it: every fault found, each with the
//! command or edit that fixes it. `main` prints them as
//! `{"error": [{"fault": ..., "fix": ...}, ...]}`.

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fault {
    pub fault: String,
    pub fix: String,
}

/// The faults a command found before it gave up: an error that carries them.
#[derive(Debug)]
pub struct Faults(pub Vec<Fault>);

impl fmt::Display for Faults {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let lines: Vec<String> = self.0.iter().map(|x| format!("{} (fix: {})", x.fault, x.fix)).collect();
        f.write_str(&lines.join("\n"))
    }
}

impl std::error::Error for Faults {}

/// An error naming one fault and its fix.
pub fn fault(fault: impl Into<String>, fix: impl Into<String>) -> anyhow::Error {
    Faults(vec![Fault { fault: fault.into(), fix: fix.into() }]).into()
}

/// The fix for an error slimfit did not foresee: it names a path, a
/// permission or a missing tool, and that is what to fix.
pub const UNFORESEEN: &str = "fix what the fault names (a path, a permission, a tool missing from PATH), then run the same \
                              slimfit command again";

/// The faults an error carries; an unforeseen error is one fault.
pub fn of(e: anyhow::Error) -> Vec<Fault> {
    match e.downcast::<Faults>() {
        Ok(f) => f.0,
        Err(e) => vec![Fault { fault: format!("{e:#}"), fix: UNFORESEEN.into() }],
    }
}

/// The error's faults, each said to be of `whose` (a suite, a file).
pub fn within(whose: &str, e: anyhow::Error) -> anyhow::Error {
    let faults = of(e).into_iter().map(|f| Fault { fault: format!("{whose}: {}", f.fault), ..f }).collect();
    Faults(faults).into()
}

/// All the results' values, or every fault of every error among them.
pub fn all<T>(results: impl IntoIterator<Item = anyhow::Result<T>>) -> anyhow::Result<Vec<T>> {
    let (mut ok, mut faults) = (Vec::new(), Vec::new());
    for r in results {
        match r {
            Ok(v) => ok.push(v),
            Err(e) => faults.extend(of(e)),
        }
    }
    if faults.is_empty() { Ok(ok) } else { Err(Faults(faults).into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Faults from several errors are kept, every one, in order; an error
    /// slimfit did not foresee still carries a fix.
    #[test]
    fn all_keeps_every_fault_and_an_unforeseen_error_gets_a_fix() {
        let got = all::<u8>([Ok(1), Err(fault("a", "fix a")), Err(anyhow::anyhow!("disk full")), Ok(2)]);
        let faults = of(got.unwrap_err());
        assert_eq!(
            faults,
            vec![
                Fault { fault: "a".into(), fix: "fix a".into() },
                Fault { fault: "disk full".into(), fix: UNFORESEEN.into() }
            ]
        );
        assert_eq!(all::<u8>([Ok(1), Ok(2)]).unwrap(), vec![1, 2]);
        assert_eq!(Faults(faults).to_string(), format!("a (fix: fix a)\ndisk full (fix: {UNFORESEEN})"));
    }
}
