# Glossary

**Run** (of a suite): where a suite's commands run and what they leave. Each command runs with `sh -c` in the suite's root and writes its log in the suite's scratch directory (`$SLIMFIT_OUT`); the suite's reports are found, cleared and read there, and a missing or unreadable one is a fault with its fix. The test run, a change's mutants and the sample all run a suite's commands through one (`measure::Run`). _Avoid_: workdir, invocation.
