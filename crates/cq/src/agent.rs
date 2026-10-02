//! Agent-specific screen interpretation. Everything that depends on an agent's UI wording lives
//! here so that UI changes in the agent (or adding Codex) touch only this module.

use serde::Serialize;

/// One selectable option of a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptOption {
    pub number: u8,
    pub label: String,
}

/// A numbered-choice prompt currently visible on screen: a tool permission request, or another
/// confirmation dialog (folder trust, development channels, questions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionPrompt {
    /// Stable id derived from text and options, used to match answers to this exact prompt.
    pub id: String,
    /// `permission` for tool permission requests, `dialog` for other confirmations.
    pub kind: String,
    /// Prompt text as displayed (tool, command, question), without the options.
    pub text: String,
    pub options: Vec<PromptOption>,
}

/// FNV-1a hash of the prompt content, as hex.
fn prompt_id(text: &str, options: &[PromptOption]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: &[u8]| {
        for x in b {
            h ^= u64::from(*x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(text.as_bytes());
    for o in options {
        feed(&[o.number]);
        feed(o.label.as_bytes());
    }
    format!("{h:016x}")
}

/// Whether a color looks like placeholder or hint text (gray), rather than typed text.
fn is_hint_color(c: vt100::Color) -> bool {
    match c {
        vt100::Color::Default => false,
        vt100::Color::Idx(i) => i == 8 || (232..=250).contains(&i),
        vt100::Color::Rgb(r, g, b) => {
            let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
            hi - lo <= 16 && hi <= 200
        }
    }
}

/// Whether Claude Code's input box is empty (only a dim placeholder after `❯`). Returns `None`
/// when no input box is visible (e.g. while a permission prompt replaces it).
pub fn claude_input_empty(screen: &vt100::Screen) -> Option<bool> {
    let (rows, cols) = screen.size();
    let text = |r: u16| screen.contents_between(r, 0, r, cols);
    // The input box is the last `❯` line directly below a solid rule.
    let row = (1..rows).rev().find(|&r| text(r).trim_start().starts_with('❯') && is_rule(&text(r - 1)))?;
    let mut r = row;
    while r < rows && !(r > row && is_rule(&text(r))) {
        let start = if r == row { text(r).find('❯').map(|i| text(r)[..i].chars().count() as u16 + 1)? } else { 0 };
        for c in start..cols {
            let Some(cell) = screen.cell(r, c) else { continue };
            if cell.has_contents() && !cell.contents().trim().is_empty() && !cell.dim() && !is_hint_color(cell.fgcolor()) {
                return Some(false);
            }
        }
        r += 1;
    }
    Some(true)
}

/// Parses a numbered option line such as `❯ 1. Yes`, `› 1. Update now` or `  2. No (esc)`.
fn parse_option(line: &str) -> Option<PromptOption> {
    let t = line.trim_start().trim_start_matches(['❯', '›', '>', ' ']).trim_end();
    let (num, rest) = t.split_once(". ")?;
    let number: u8 = num.parse().ok()?;
    if !(1..=9).contains(&number) || rest.is_empty() {
        return None;
    }
    Some(PromptOption { number, label: rest.trim().to_owned() })
}

/// A solid horizontal rule, which Claude Code draws above a prompt box. Dashed rules (`╌`) are
/// separators inside the box and are not boundaries.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 10 && t.chars().all(|c| matches!(c, '─' | '━'))
}

/// Collects consecutive numbered options starting at `lines[0]` (blank lines before the first are
/// skipped; indented continuation lines extend the previous label).
fn collect_options(lines: &[&str]) -> Vec<PromptOption> {
    let mut options = Vec::new();
    for l in lines {
        if l.trim().is_empty() && options.is_empty() {
            continue;
        }
        match parse_option(l) {
            Some(o) if o.number as usize == options.len() + 1 => options.push(o),
            Some(_) => break,
            // Long labels may wrap onto an indented continuation line.
            None if !options.is_empty() && l.starts_with("     ") && !l.trim().is_empty() => {
                let last: &mut PromptOption = options.last_mut().unwrap();
                last.label.push(' ');
                last.label.push_str(l.trim());
            }
            None => break,
        }
    }
    options
}

/// Joins the lines that describe a prompt: from the nearest solid rule above `end` (at most
/// `max` lines up) through `end` inclusive.
fn prompt_text(lines: &[&str], end: usize, max: usize) -> String {
    let start = lines[..end].iter().rposition(|l| is_rule(l)).map(|i| i + 1).unwrap_or(end.saturating_sub(max));
    let start = start.max(end.saturating_sub(max));
    lines[start..=end].iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n").trim().to_owned()
}

fn prompt(kind: &str, text: String, options: Vec<PromptOption>) -> PermissionPrompt {
    PermissionPrompt { id: prompt_id(&text, &options), kind: kind.to_owned(), text, options }
}

/// The key hint under a selection dialog: Claude Code's "Enter to confirm · Esc to cancel", or
/// Codex's "enter confirm · esc skip" / "enter continue".
fn is_dialog_footer(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("enter to confirm") || (l.contains("enter") && (l.contains("confirm") || l.contains("continue")) && l.contains("esc"))
}

/// Finds a prompt that waits for a numbered choice (Claude Code and Codex): a permission request ("Do you
/// want to ...?" followed by options), or another dialog whose options end with an
/// "Enter to confirm" footer.
pub fn find_choice_prompt(screen: &str) -> Option<PermissionPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    if let Some(q) = lines.iter().rposition(|l| l.contains("Do you want to")) {
        let options = collect_options(&lines[q + 1..]);
        if options.len() >= 2 {
            return Some(prompt("permission", prompt_text(&lines, q, 20), options));
        }
    }
    let footer = lines.iter().rposition(|l| is_dialog_footer(l))?;
    let first = (footer.saturating_sub(15)..footer).find(|&i| parse_option(lines[i]).is_some_and(|o| o.number == 1))?;
    let options = collect_options(&lines[first..footer]);
    if options.len() < 2 || first == 0 {
        return None;
    }
    let text_end = (0..first).rev().find(|&i| !lines[i].trim().is_empty())?;
    Some(prompt("dialog", prompt_text(&lines, text_end, 12), options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bash_prompt() {
        let screen = "\
● Running ls
────────────────────────────────────────
 Bash command

   ls -la
   List files

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for ls commands in
      C:\\work
   3. No, and tell Claude what to do differently (esc)

";
        let p = find_choice_prompt(screen).unwrap();
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[0].label, "Yes");
        assert!(p.options[1].label.ends_with("in C:\\work"));
        assert!(p.text.starts_with("Bash command"));
        assert!(p.text.ends_with("Do you want to proceed?"));
    }

    #[test]
    fn keeps_tool_and_command_above_dashed_separators() {
        // Layout observed with Claude Code 2.1.286.
        let screen = "\
● Running…
  ⎿  $ powershell -NoProfile -Command 1+1
────────────────────────────────────────────────────
 Bash command
 Run the PowerShell command
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 powershell -NoProfile -Command 1+1
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 This command requires approval

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for: powershell -NoProfile -Command 1+1
   3. No

 Esc to cancel · Tab to amend
";
        let p = find_choice_prompt(screen).unwrap();
        assert!(p.text.starts_with("Bash command"), "{}", p.text);
        assert!(p.text.contains("powershell -NoProfile -Command 1+1"));
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[2].label, "No");
    }

    fn screen_of(bytes: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(10, 60, 0);
        p.process(bytes.as_bytes());
        p
    }

    #[test]
    fn input_box_with_placeholder_is_empty() {
        let rule = "─".repeat(50);
        let p = screen_of(&format!("hello\r\n{rule}\r\n❯ \x1b[38;2;153;153;153mTry \"fix lint\"\x1b[0m\r\n{rule}\r\n  ? for shortcuts"));
        assert_eq!(claude_input_empty(p.screen()), Some(true));
        let p = screen_of(&format!("{rule}\r\n❯ \x1b[2mTry this\x1b[0m\r\n{rule}"));
        assert_eq!(claude_input_empty(p.screen()), Some(true));
    }

    #[test]
    fn input_box_with_text_is_not_empty() {
        let rule = "─".repeat(50);
        let p = screen_of(&format!("{rule}\r\n❯ fix the build\r\n{rule}"));
        assert_eq!(claude_input_empty(p.screen()), Some(false));
        let p = screen_of(&format!("{rule}\r\n❯ \r\n  second line\r\n{rule}"));
        assert_eq!(claude_input_empty(p.screen()), Some(false));
        assert_eq!(claude_input_empty(screen_of("no box here").screen()), None);
    }

    #[test]
    fn prompt_id_tracks_content() {
        let o = vec![PromptOption { number: 1, label: "Yes".into() }];
        assert_eq!(prompt_id("a", &o), prompt_id("a", &o));
        assert_ne!(prompt_id("a", &o), prompt_id("b", &o));
    }

    #[test]
    fn detects_confirmation_dialogs() {
        let screen = "\
 WARNING: Loading development channels

 --dangerously-load-development-channels is for local channel development only.
 Please use --channels to run a list of approved channels.

 Channels: server:cq

 ❯ 1. I am using this for local development
   2. Exit

 Enter to confirm · Esc to cancel
";
        let p = find_choice_prompt(screen).unwrap();
        assert_eq!(p.kind, "dialog");
        assert_eq!(p.options.len(), 2);
        assert!(p.text.starts_with("WARNING: Loading development channels"), "{}", p.text);
        assert!(p.text.ends_with("Channels: server:cq"));
    }

    #[test]
    fn detects_codex_selection_dialogs() {
        let screen = "\
  Hooks need review
  4 hooks are new or changed.
  Hooks can run outside the sandbox after you trust them.

› 1. Review hooks
  2. Trust all and continue
  3. Continue without trusting (hooks won't run)

  enter confirm · esc skip
";
        let p = find_choice_prompt(screen).unwrap();
        assert_eq!(p.kind, "dialog");
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[1].label, "Trust all and continue");
        assert!(p.text.starts_with("Hooks need review"));
    }

    #[test]
    fn ignores_plain_screen() {
        assert!(find_choice_prompt("❯ hello\n1. not a prompt\n").is_none());
    }
}
