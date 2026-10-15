//! External subcommands: `cq NAME ARGS…` runs `cq-NAME ARGS…` (like git and cargo), so plugins
//! such as cq-orchestrator are reachable as `cq orchestrator` without the core knowing about them. Also the
//! registry of installed plugins and the commands their manifests declare.

use crate::{config, paths};
use anyhow::{Context, Result, bail};
use cq_protocol::{CommandArg, CommandInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    let Some(found) = find_plugin(name) else {
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

/// Directory of the running `cq` executable; plugins placed here are installed with it.
fn own_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
}

/// Finds `cq-<name>` next to this executable, then in `PATH`.
pub fn find_plugin(name: &str) -> Option<PathBuf> {
    paths::find_program(&format!("cq-{name}"), own_dir().into_iter().chain(paths::path_dirs()))
}

/// What a plugin prints for `cq-NAME manifest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub commands: Vec<ManifestCommand>,
}

/// A command declared by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestCommand {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub args: Vec<CommandArg>,
    #[serde(default)]
    pub keybinding: Option<String>,
    pub run: Run,
}

/// How the daemon carries out a manifest command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Run {
    /// Run `cq-NAME ARGS…` and report how it ended.
    Exec(Vec<String>),
    /// Start `cq-NAME ARGS…` unless a client already handles the command, and hand the
    /// invocation to it once it registers.
    Activate(Vec<String>),
}

/// An installed plugin (`~/.codequilt/plugins/NAME.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub path: PathBuf,
    /// Installed together with `cq` (found next to it) rather than with `cq plugin add`.
    #[serde(default)]
    pub bundled: bool,
    pub manifest: Manifest,
}

impl ManifestCommand {
    /// The command as announced to clients.
    pub fn info(&self, plugin: &str) -> CommandInfo {
        CommandInfo {
            name: self.name.clone(),
            title: self.title.clone(),
            palette: true,
            plugin: Some(plugin.to_owned()),
            args: self.args.clone(),
            keybinding: self.keybinding.clone(),
        }
    }
}

fn plugins_dir() -> Result<PathBuf> {
    Ok(paths::root()?.join("plugins"))
}

/// Runs `<path> manifest` and parses its output.
fn read_manifest(path: &Path) -> Result<Manifest> {
    let (program, prefix) = crate::host::with_batch_wrapper(path);
    let out = std::process::Command::new(program)
        .args(prefix)
        .arg("manifest")
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("run {} manifest", path.display()))?;
    if !out.status.success() {
        bail!("{} manifest failed: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("invalid manifest from {}", path.display()))
}

fn save(name: &str, plugin: &Installed) -> Result<()> {
    let dir = plugins_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec_pretty(plugin)?)?;
    Ok(())
}

/// Installed plugins by name.
pub fn installed() -> Vec<(String, Installed)> {
    let Ok(entries) = plugins_dir().and_then(|d| Ok(std::fs::read_dir(d)?)) else { return Vec::new() };
    let mut out: Vec<(String, Installed)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_str()?.to_owned();
            if p.extension()? != "json" {
                return None;
            }
            let plugin = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
            Some((name, plugin))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Plugin names of the `cq-*` executables next to this one.
fn bundled_names() -> Vec<String> {
    let Some(dir) = own_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let stem = if cfg!(windows) { file.strip_suffix(".exe")? } else { file.as_str() };
            let name = stem.strip_prefix("cq-")?;
            (!name.is_empty() && !name.contains('.')).then(|| name.to_owned())
        })
        .filter(|name| paths::find_program(&format!("cq-{name}"), [dir.clone()]).is_some())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Records the manifests of plugins installed next to `cq`, re-reading one when its executable
/// is newer than the record. Plugins added with `cq plugin add` are left alone.
pub fn sync_bundled() {
    let records: std::collections::HashMap<String, Installed> = installed().into_iter().collect();
    for name in bundled_names() {
        let Some(path) = own_dir().and_then(|d| paths::find_program(&format!("cq-{name}"), [d])) else { continue };
        let record_file = plugins_dir().map(|d| d.join(format!("{name}.json")));
        let stale = match (records.get(&name), record_file) {
            (Some(r), _) if !r.bundled => false,
            (Some(r), Ok(file)) => r.path != path || modified(&path) > modified(&file),
            _ => true,
        };
        if !stale {
            continue;
        }
        match read_manifest(&path) {
            Ok(manifest) => {
                let _ = save(&name, &Installed { path, bundled: true, manifest });
            }
            Err(e) => eprintln!("plugin {name}: {e:#}"),
        }
    }
}

fn modified(p: &Path) -> Option<std::time::SystemTime> {
    p.metadata().and_then(|m| m.modified()).ok()
}

/// `cq plugin add NAME`.
pub fn add(name: &str) -> Result<()> {
    let Some(path) = find_plugin(name) else { bail!("no cq-{name} next to cq or in PATH") };
    let manifest = read_manifest(&path)?;
    let count = manifest.commands.len();
    save(name, &Installed { path: path.clone(), bundled: false, manifest })?;
    println!("added {name} ({}) with {count} command(s)", path.display());
    Ok(())
}

/// `cq plugin remove NAME`.
pub fn remove(name: &str) -> Result<()> {
    let file = plugins_dir()?.join(format!("{name}.json"));
    if !file.exists() {
        bail!("plugin '{name}' is not installed");
    }
    std::fs::remove_file(file)?;
    if bundled_names().iter().any(|n| n == name) {
        println!("removed {name}; it is installed next to cq and comes back when the daemon starts");
    } else {
        println!("removed {name}");
    }
    Ok(())
}

/// `cq plugin refresh`: re-reads every manifest.
pub fn refresh() -> Result<()> {
    for (name, mut plugin) in installed() {
        match read_manifest(&plugin.path) {
            Ok(m) => {
                plugin.manifest = m;
                save(&name, &plugin)?;
                println!("refreshed {name}");
            }
            Err(e) => eprintln!("{name}: {e:#}"),
        }
    }
    sync_bundled();
    Ok(())
}

/// `cq plugin list`.
pub fn list() -> Result<()> {
    sync_bundled();
    let all = installed();
    if all.is_empty() {
        println!("no plugins installed");
    }
    for (name, p) in all {
        let how = if p.bundled { "bundled" } else { "added" };
        println!("{name:<12} {how:<8} {}", p.path.display());
        for c in &p.manifest.commands {
            println!("  {:<24} {}", c.name, c.title);
        }
    }
    Ok(())
}

/// Expands `${arg}` placeholders from the invocation arguments, argument defaults and the
/// invocation context (`${machine}`, `${session}`).
pub fn expand_args(
    template: &[String],
    declared: &[CommandArg],
    given: &serde_json::Value,
    context: Option<&cq_protocol::SessionRef>,
) -> Result<Vec<String>> {
    let lookup = |key: &str| -> Option<String> {
        match key {
            "machine" => return context.map(|c| crate::machine::name_of(&c.machine)),
            "session" => return context.map(|c| c.session.clone()),
            _ => {}
        }
        given
            .get(key)
            .and_then(|v| v.as_str().map(str::to_owned).or_else(|| (!v.is_null()).then(|| v.to_string())))
            .or_else(|| declared.iter().find(|a| a.name == key).and_then(|a| a.default.clone()))
    };
    template
        .iter()
        .map(|part| {
            let mut out = String::new();
            let mut rest = part.as_str();
            while let Some(start) = rest.find("${") {
                out.push_str(&rest[..start]);
                let Some(len) = rest[start..].find('}') else { bail!("unclosed placeholder in '{part}'") };
                let key = &rest[start + 2..start + len];
                out.push_str(&lookup(key).with_context(|| format!("missing argument '{key}'"))?);
                rest = &rest[start + len + 1..];
            }
            out.push_str(rest);
            Ok(out)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Vec<String> {
        let aliases = HashMap::from([
            ("o".to_owned(), "orchestrator".to_owned()),
            ("os".to_owned(), "orchestrator start main".to_owned()),
            ("ls".to_owned(), "orchestrator".to_owned()),
        ]);
        let args = args.iter().map(OsString::from).collect();
        expand_with(args, |n| n == "ls", &aliases).into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn expands_alias_in_subcommand_position() {
        assert_eq!(run(&["cq", "o", "start"]), ["cq", "orchestrator", "start"]);
        assert_eq!(run(&["cq", "os", "--detach"]), ["cq", "orchestrator", "start", "main", "--detach"]);
    }

    #[test]
    fn leaves_builtins_flags_and_later_words_alone() {
        assert_eq!(run(&["cq", "ls"]), ["cq", "ls"]);
        assert_eq!(run(&["cq", "--help"]), ["cq", "--help"]);
        assert_eq!(run(&["cq", "attach", "q"]), ["cq", "attach", "q"]);
        assert_eq!(run(&["cq"]), ["cq"]);
    }

    fn arg(name: &str, default: Option<&str>) -> CommandArg {
        CommandArg { name: name.into(), title: String::new(), default: default.map(Into::into) }
    }

    #[test]
    fn expands_given_default_and_context_values() {
        let template = ["start".to_owned(), "${name}".to_owned(), "--in=${machine}/${session}".to_owned()];
        let declared = [arg("name", Some("orchestrator"))];
        let context = cq_protocol::SessionRef { machine: "wsl".into(), session: "abc".into() };
        let given = serde_json::json!({ "name": "main" });
        assert_eq!(
            expand_args(&template, &declared, &given, Some(&context)).unwrap(),
            ["start", "main", "--in=wsl/abc"]
        );
        let defaults = expand_args(&template[..2], &declared, &serde_json::json!({}), None).unwrap();
        assert_eq!(defaults, ["start", "orchestrator"]);
    }

    #[test]
    fn missing_values_are_errors() {
        let err = expand_args(&["${session}".to_owned()], &[], &serde_json::json!({}), None).unwrap_err();
        assert!(err.to_string().contains("session"));
        assert!(expand_args(&["${oops".to_owned()], &[], &serde_json::json!({}), None).is_err());
    }
}
