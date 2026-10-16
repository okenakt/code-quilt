//! Files an agent's reply shows the user: the files it mentions and its diagram and math blocks,
//! listed beside its terminal on the board. The rules are in `docs/design.md`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Extensions the board renders.
const VIEWABLE: [&str; 13] =
    ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "pdf", "html", "htm", "md", "markdown"];

/// Code block languages that are written out to files.
const BLOCKS: [&str; 2] = ["mermaid", "math"];

/// Directory entries searched for a file name, so a session started in a huge directory stays quick.
const MAX_WALK: usize = 50_000;

/// Characters that end a mention besides white space, including Japanese brackets and punctuation.
const DELIMITERS: &str = "`'\"()<>[]{},;|*（）「」『』【】、。，．：";

/// Absolute paths of the files `reply` shows, in order: the mentioned files that exist, then its
/// blocks written to `blocks_dir`.
pub fn files(reply: &str, cwd: &Path, blocks_dir: &Path) -> Vec<String> {
    let mut index = None;
    let mentioned = mentions(reply).into_iter().filter_map(|m| resolve(m, cwd, &mut index));
    let written = blocks(reply).into_iter().filter_map(|(kind, body)| write_block(blocks_dir, kind, &body).ok());
    let mut out: Vec<String> = Vec::new();
    for path in mentioned.chain(written).filter_map(|p| shown(&p)) {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// The canonical path of a file the board can read back, if it is one.
fn shown(path: &Path) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() <= cq_protocol::MAX_SHARED_FILE_LEN)
        .then(|| crate::config::display_path(&path).to_string_lossy().into_owned())
}

fn viewable(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| VIEWABLE.contains(&e.to_ascii_lowercase().as_str()))
}

/// Words that look like paths of viewable files, each cut after its extension so that a `:line`
/// location or Japanese text right after the name (`design.mdを更新`) is left out.
fn mentions(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || DELIMITERS.contains(c)) {
        let mut rest = word.trim_start_matches('@');
        while let Some(cut) = end_of_name(rest) {
            if !rest[..cut].contains("://") {
                out.push(&rest[..cut]);
            }
            rest = &rest[cut..];
        }
    }
    out
}

/// Where the first viewable extension in `t` ends.
fn end_of_name(t: &str) -> Option<usize> {
    t.match_indices('.').find_map(|(i, _)| {
        let ext = &t[i + 1..];
        let len = ext.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(ext.len());
        VIEWABLE.contains(&ext[..len].to_ascii_lowercase().as_str()).then_some(i + 1 + len)
    })
}

/// Finds the file a mention means, trying it whole and then the part after any Japanese text
/// before it (`設定はdocs/design.md`).
fn resolve(mention: &str, cwd: &Path, index: &mut Option<Vec<PathBuf>>) -> Option<PathBuf> {
    let tail =
        mention.rfind(|c: char| !c.is_ascii()).map(|i| &mention[i + mention[i..].chars().next().unwrap().len_utf8()..]);
    std::iter::once(mention).chain(tail.filter(|t| !t.is_empty())).find_map(|m| resolve_one(m, cwd, index))
}

/// The path itself, else the newest file under `cwd` whose path ends with it (an agent usually
/// names files without their directory).
fn resolve_one(mention: &str, cwd: &Path, index: &mut Option<Vec<PathBuf>>) -> Option<PathBuf> {
    let path = match mention.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => PathBuf::from(mention),
    };
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let direct = cwd.join(&path);
    if direct.is_file() {
        return Some(direct);
    }
    let parts: Vec<&str> = mention.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect();
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    index.get_or_insert_with(|| walk(cwd)).iter().filter(|p| ends_with(p, &parts)).max_by_key(|p| modified(p)).cloned()
}

fn ends_with(path: &Path, parts: &[&str]) -> bool {
    let names: Vec<_> = path.components().rev().take(parts.len()).map(|c| c.as_os_str()).collect();
    names.len() == parts.len() && names.iter().zip(parts.iter().rev()).all(|(n, p)| *n == std::ffi::OsStr::new(p))
}

/// Viewable files under `cwd`, skipping hidden and ignored ones as git would.
fn walk(cwd: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(cwd)
        .require_git(false)
        .build()
        .take(MAX_WALK)
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()) && viewable(e.path()))
        .map(ignore::DirEntry::into_path)
        .collect()
}

/// Diagram and math blocks: fenced ```` ```mermaid ```` / ```` ```math ```` blocks and `$$ … $$`
/// starting a line. Other fenced blocks are skipped whole.
fn blocks(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if let Some(mark) = ['`', '~'].into_iter().find(|&c| t.starts_with([c; 3])) {
            let fence = &t[..t.len() - t.trim_start_matches(mark).len()];
            let lang = t[fence.len()..].split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            let body: Vec<&str> =
                lines.by_ref().take_while(|l| !l.trim_start().starts_with(fence)).map(|l| dedent(l, indent)).collect();
            if let Some(kind) = BLOCKS.into_iter().find(|k| *k == lang) {
                out.push((kind, body.join("\n")));
            }
        } else if let Some(mut rest) = t.strip_prefix("$$") {
            let mut body = Vec::new();
            loop {
                if let Some(end) = rest.find("$$") {
                    body.push(&rest[..end]);
                    break;
                }
                body.push(rest);
                match lines.next() {
                    Some(l) => rest = l,
                    None => {
                        body.clear();
                        break;
                    }
                }
            }
            let math = body.join("\n").trim().to_owned();
            if !math.is_empty() {
                out.push(("math", math));
            }
        }
    }
    out
}

fn dedent(line: &str, indent: usize) -> &str {
    let spaces = line.len() - line.trim_start().len();
    &line[spaces.min(indent)..]
}

/// Writes a block as a Markdown file named by its contents, so the same block maps to one file.
fn write_block(dir: &Path, kind: &str, body: &str) -> std::io::Result<PathBuf> {
    let text = if kind == "math" { format!("$$\n{body}\n$$\n") } else { format!("```{kind}\n{body}\n```\n") };
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    let path = dir.join(format!("{kind}-{hash:016x}.md"));
    if !path.is_file() {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mentions_in_prose() {
        let text = "`docs/design.md:162` と 設定（features.md）、decisions.mdとdocs/a.mdを更新し、out.png を作りました。\n\
                    See [the plot](assets/fig.svg), @notes.md, https://example.com/a.png, v1.2.png and src/main.rs.";
        assert_eq!(
            mentions(text),
            [
                "docs/design.md",
                "features.md",
                "decisions.md",
                "とdocs/a.md",
                "out.png",
                "assets/fig.svg",
                "notes.md",
                "v1.2.png"
            ]
        );
    }

    #[test]
    fn extracts_diagram_and_math_blocks() {
        let text = "Intro\n  ```mermaid\n  flowchart LR\n    A --> B\n  ```\n```sh\n$$ not math $$\n```\n\
                    $$\na^2 + b^2\n$$\n$$ x = 1 $$\n```math\n\\int f\n```\n```rust\nfn main() {}\n```\n$$ unclosed";
        let expected =
            [("mermaid", "flowchart LR\n  A --> B"), ("math", "a^2 + b^2"), ("math", "x = 1"), ("math", "\\int f")];
        assert_eq!(blocks(text), expected.map(|(k, b)| (k, b.to_owned())));
    }

    #[test]
    fn resolves_names_to_the_newest_existing_file() {
        let root = std::env::temp_dir().join(format!("cq-reply-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (cwd, blocks_dir) = (root.join("repo"), root.join("blocks"));
        for dir in ["a", "b", "target"] {
            std::fs::create_dir_all(cwd.join(dir)).unwrap();
        }
        std::fs::write(cwd.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(cwd.join("a/index.html"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(cwd.join("b/index.html"), "new").unwrap();
        std::fs::write(cwd.join("target/report.md"), "ignored").unwrap();
        std::fs::write(cwd.join("a/plot.png"), "png").unwrap();

        let reply = "index.html, a/index.html, 図はplot.pngです, missing.md, report.md\n```mermaid\nA\n```";
        let found = files(reply, &cwd, &blocks_dir);
        let shown_as = |p: PathBuf| shown(&p).unwrap();
        let block = shown_as(std::fs::read_dir(&blocks_dir).unwrap().next().unwrap().unwrap().path());
        assert_eq!(
            found,
            [
                shown_as(cwd.join("b/index.html")),
                shown_as(cwd.join("a/index.html")),
                shown_as(cwd.join("a/plot.png")),
                block.clone()
            ]
        );
        assert_eq!(std::fs::read_to_string(&block).unwrap(), "```mermaid\nA\n```\n");
        let _ = std::fs::remove_dir_all(&root);
    }
}
