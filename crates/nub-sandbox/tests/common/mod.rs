//! Shared test scaffolding: a deterministic `$(…)` runner and a `CompileCtx`
//! builder over fixed home anchors + a controlled ambient env.

use nub_sandbox::CommandRunner;
use nub_sandbox::compiler::CompileCtx;
use nub_sandbox::matcher::Homes;
use std::collections::BTreeMap;

/// A `$(…)` runner that echoes a deterministic marker so substitution is testable
/// without spawning a real shell.
pub struct StubRunner;
impl CommandRunner for StubRunner {
    fn run(&self, command: &str) -> Result<String, String> {
        // A couple of recognized commands return fixed values; everything else
        // echoes a marker so a test can assert the command reached the runner.
        match command {
            "echo hi" => Ok("hi\n".to_string()),
            "fail" => Err("stub failure".to_string()),
            other => Ok(format!("STUB[{other}]")),
        }
    }
}

/// Fixed, OS-agnostic home anchors for compiler/IR tests. Absolute so the glob
/// prefix-canonicalization has something to anchor on.
pub fn homes() -> Homes {
    #[cfg(windows)]
    {
        Homes {
            home: "C:/Users/u".into(),
            tmp: "C:/Temp".into(),
            cache: "C:/Users/u/.cache".into(),
            project: "C:/proj".into(),
        }
    }
    #[cfg(not(windows))]
    {
        Homes {
            home: "/home/u".into(),
            tmp: "/tmp".into(),
            cache: "/home/u/.cache".into(),
            project: "/proj".into(),
        }
    }
}

/// Build a ctx with the given trust + ambient env pairs.
pub fn ctx(trusted: bool, env: &[(&str, &str)]) -> CompileCtx {
    let ambient: BTreeMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    CompileCtx {
        homes: homes(),
        cwd: homes().project,
        trusted,
        ambient_env: ambient,
        runner: Box::new(StubRunner),
    }
}
