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
    /// For lists without numbers: the option currently selected. They are answered by moving
    /// the selection with the arrow keys instead of typing the number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<u8>,
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
    // Without colors the placeholder looks like typed text; recognise Claude Code's own hint.
    let first = text(row);
    let after = first.trim_start().trim_start_matches('❯').trim();
    let next_is_rule = row + 1 < rows && is_rule(&text(row + 1));
    if next_is_rule && after.starts_with("Try \"") && after.ends_with('"') {
        return Some(true);
    }
    input_empty_from(screen, row, '❯', |r| is_rule(&text(r)))
}

/// Whether Codex's composer is empty (only a dim placeholder after `›`). The composer is the last
/// `›` line, continued by the following lines up to a blank one.
pub fn codex_input_empty(screen: &vt100::Screen) -> Option<bool> {
    let (rows, cols) = screen.size();
    let text = |r: u16| screen.contents_between(r, 0, r, cols);
    let row = (0..rows).rev().find(|&r| text(r).trim_start().starts_with('›'))?;
    input_empty_from(screen, row, '›', |r| text(r).trim().is_empty())
}

/// Whether an input box starting at `row` after `marker` and ending before the first later row
/// for which `end` holds shows only dim or hint-colored text.
fn input_empty_from(screen: &vt100::Screen, row: u16, marker: char, end: impl Fn(u16) -> bool) -> Option<bool> {
    let (rows, cols) = screen.size();
    let first = screen.contents_between(row, 0, row, cols);
    let start = first[..first.find(marker)?].chars().count() as u16 + 1;
    let mut r = row;
    while r < rows && !(r > row && end(r)) {
        for c in if r == row { start } else { 0 }..cols {
            let Some(cell) = screen.cell(r, c) else { continue };
            if cell.has_contents() && !cell.contents().trim().is_empty() && !cell.dim() && !is_hint_color(cell.fgcolor()) {
                return Some(false);
            }
        }
        r += 1;
    }
    Some(true)
}

/// Developer aid: prints the cells after `❯` on each input-looking row with their attributes.
pub fn debug_input_row(screen: &vt100::Screen) {
    let (rows, cols) = screen.size();
    for r in 0..rows {
        let line = screen.contents_between(r, 0, r, cols);
        if !line.trim_start().starts_with(['❯', '›']) {
            continue;
        }
        let above = if r > 0 { screen.contents_between(r - 1, 0, r - 1, cols) } else { String::new() };
        println!("row {r}: rule above: {} | {}", is_rule(&above), line.trim_end());
        for c in 0..cols.min(30) {
            if let Some(cell) = screen.cell(r, c) {
                if cell.has_contents() && !cell.contents().trim().is_empty() {
                    println!("  col {c} {:?} fg={:?} dim={} inv={}", cell.contents(), cell.fgcolor(), cell.dim(), cell.inverse());
                }
            }
        }
    }
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

fn prompt(kind: &str, text: String, options: Vec<PromptOption>, selected: Option<u8>) -> PermissionPrompt {
    PermissionPrompt { id: prompt_id(&text, &options), kind: kind.to_owned(), text, options, selected }
}

/// An unnumbered list ending at the last line of `lines`: consecutive lines, one of them marked
/// as selected with `❯` or `›`. Returns the index of its first line, the options numbered from
/// the top, and the selected number.
fn marked_options(lines: &[&str]) -> Option<(usize, Vec<PromptOption>, u8)> {
    let end = lines.iter().rposition(|l| !l.trim().is_empty())?;
    let start = (0..=end).rev().take_while(|&i| !lines[i].trim().is_empty()).last()?;
    let block = &lines[start..=end];
    let marker = |l: &str| l.trim_start().starts_with(['❯', '›']);
    let selected = block.iter().position(|l| marker(l))?;
    if !(2..=9).contains(&block.len()) {
        return None;
    }
    let options = block
        .iter()
        .enumerate()
        .map(|(i, l)| PromptOption { number: i as u8 + 1, label: l.trim().trim_start_matches(['❯', '›']).trim().to_owned() })
        .collect();
    Some((start, options, selected as u8 + 1))
}

/// The key hint under a selection dialog: Claude Code's "Enter to confirm · Esc to cancel", or
/// Codex's "enter confirm · esc skip" / "enter continue".
fn is_dialog_footer(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("enter to confirm") || (l.contains("enter") && (l.contains("confirm") || l.contains("continue")) && l.contains("esc"))
}

/// Finds a prompt that waits for a choice (Claude Code and Codex): a permission request ("Do you
/// want to ...?" followed by options), or another dialog whose options end with an
/// "Enter to confirm" footer.
pub fn find_choice_prompt(screen: &str) -> Option<PermissionPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    if let Some(q) = lines.iter().rposition(|l| l.contains("Do you want to")) {
        let options = collect_options(&lines[q + 1..]);
        if options.len() >= 2 {
            return Some(prompt("permission", prompt_text(&lines, q, 20), options, None));
        }
    }
    let footer = lines.iter().rposition(|l| is_dialog_footer(l))?;
    let numbered = (footer.saturating_sub(15)..footer).find(|&i| parse_option(lines[i]).is_some_and(|o| o.number == 1));
    let (first, options, selected) = match numbered {
        Some(first) => (first, collect_options(&lines[first..footer]), None),
        None => {
            let (first, options, selected) = marked_options(&lines[footer.saturating_sub(10)..footer])?;
            (first + footer.saturating_sub(10), options, Some(selected))
        }
    };
    if options.len() < 2 || first == 0 {
        return None;
    }
    let text_end = (0..first).rev().find(|&i| !lines[i].trim().is_empty())?;
    // Codex asks for tool approval without "Do you want to"; its options give it away.
    let is_permission = options.iter().any(|o| o.label.starts_with("Yes, proceed") || o.label.starts_with("Yes, and don't ask again"));
    let kind = if is_permission { "permission" } else { "dialog" };
    Some(prompt(kind, prompt_text(&lines, text_end, 12), options, selected))
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
    fn codex_composer_is_the_last_prompt_line() {
        let history = "› earlier prompt\r\n  reply\r\n\r\n";
        let footer = "\r\n\r\n  GPT medium · ~\\repo";
        let p = screen_of(&format!("{history}› \x1b[2mAsk Codex to do anything\x1b[0m{footer}"));
        assert_eq!(codex_input_empty(p.screen()), Some(true));
        let p = screen_of(&format!("{history}› line one\r\n  line two{footer}"));
        assert_eq!(codex_input_empty(p.screen()), Some(false));
    }

    #[test]
    fn colorless_placeholder_is_empty() {
        let rule = "─".repeat(50);
        let p = screen_of(&format!("{rule}\r\n❯ Try \"how do I log an error?\"\r\n{rule}"));
        assert_eq!(claude_input_empty(p.screen()), Some(true));
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
        assert_eq!(p.selected, None);

        // Folder trust (Claude Code 2.1.287) lists its options without numbers.
        let screen = "\
 Accessing workspace:

 C:\\Users\\me\\orchestrators\\orchestrator

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";
        let p = find_choice_prompt(screen).unwrap();
        assert_eq!(p.kind, "dialog");
        assert_eq!(p.options[1].label, "Yes, I trust this folder");
        assert_eq!((p.options.len(), p.selected), (2, Some(1)));
        assert!(p.text.starts_with("Accessing workspace:") && p.text.ends_with("Security guide"), "{}", p.text);
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
