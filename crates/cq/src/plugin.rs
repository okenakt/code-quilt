//! External subcommands: `cq NAME ARGS…` runs `cq-NAME ARGS…` (like git and cargo), so plugins
//! such as cq-queen are reachable as `cq queen` without the core knowing about them.

use crate::{config, paths};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::ffi::OsString;

/// Replaces an alias in the subcommand position with its expansion. Built-in subcommands and
/// their aliases always win, so an alias cannot change what an existing command does.
pub fn expand_alias(args: Vec<OsString>, is_builtin: impl Fn(&str) -> bool) -> Vec<OsString> {
    expand_with(args, is_builtin, &config::aliases())
}

fn expand_with(
    args: Vec<OsString>,
    is_builtin: impl Fn(&str) -> bool,
    aliases: &HashMap<String, String>,
) -> Vec<OsString> {
    let Some(first) = args.get(1).and_then(|a| a.to_str()) else { return args };
    if first.starts_with('-') || is_builtin(first) {
        return args;
    }
    let Some(expansion) = aliases.get(first) else { return args };
    let mut out = vec![args[0].clone()];
    out.extend(expansion.split_whitespace().map(OsString::from));
    out.extend(args.into_iter().skip(2));
    out
}

/// Runs `cq-<name>` with `args` and returns its exit code. The plugin is looked up next to this
/// executable first, then in `PATH`.
pub fn run_external(name: &str, args: &[OsString]) -> Result<i32> {
    let prog = format!("cq-{name}");
    let own_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf()));
    let Some(found) = paths::find_program(&prog, own_dir.into_iter().chain(paths::path_dirs())) else {
        bail!("unknown command '{name}' (no {prog} next to cq or in PATH)");
    };
    let (program, prefix) = crate::host::with_batch_wrapper(&found);
    let mut cmd = std::process::Command::new(program);
    cmd.args(prefix).args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        bail!("run {prog}: {err}");
    }
    #[cfg(not(unix))]
    {
        ignore_ctrl_c();
        let status = cmd.status().map_err(|e| anyhow::anyhow!("run {prog}: {e}"))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// Keeps this process alive on Ctrl+C so the plugin, which shares the console, handles it and
/// the shell prompt does not return while the plugin is still running. A handler routine (unlike
/// ignoring Ctrl+C outright) is not inherited by the child.
#[cfg(windows)]
fn ignore_ctrl_c() {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
    unsafe extern "system" fn handled(ctrl: u32) -> i32 {
        i32::from(ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT)
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handled), 1);
    }
}

#[cfg(not(any(unix, windows)))]
fn ignore_ctrl_c() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Vec<String> {
        let aliases = HashMap::from([
            ("q".to_owned(), "queen".to_owned()),
            ("qs".to_owned(), "queen start main".to_owned()),
            ("ls".to_owned(), "queen".to_owned()),
        ]);
        let args = args.iter().map(OsString::from).collect();
        expand_with(args, |n| n == "ls", &aliases).into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn expands_alias_in_subcommand_position() {
        assert_eq!(run(&["cq", "q", "start"]), ["cq", "queen", "start"]);
        assert_eq!(run(&["cq", "qs", "--detach"]), ["cq", "queen", "start", "main", "--detach"]);
    }

    #[test]
    fn leaves_builtins_flags_and_later_words_alone() {
        assert_eq!(run(&["cq", "ls"]), ["cq", "ls"]);
        assert_eq!(run(&["cq", "--help"]), ["cq", "--help"]);
        assert_eq!(run(&["cq", "attach", "q"]), ["cq", "attach", "q"]);
        assert_eq!(run(&["cq"]), ["cq"]);
    }
}
