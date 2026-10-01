//! Agent-specific screen interpretation. Everything that depends on an agent's UI wording lives
//! here so that UI changes in the agent (or adding Codex) touch only this module.

use serde::Serialize;

/// One selectable option of a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptOption {
    pub number: u8,
    pub label: String,
}

/// A permission prompt currently visible on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionPrompt {
    /// Prompt text as displayed (tool, command, question), without the options.
    pub text: String,
    pub options: Vec<PromptOption>,
}

/// Parses a numbered option line such as `❯ 1. Yes` or `  2. No (esc)`.
fn parse_option(line: &str) -> Option<PromptOption> {
    let t = line.trim_start().trim_start_matches(['❯', '>', ' ']).trim_end();
    let (num, rest) = t.split_once(". ")?;
    let number: u8 = num.parse().ok()?;
    if !(1..=9).contains(&number) || rest.is_empty() {
        return None;
    }
    Some(PromptOption { number, label: rest.trim().to_owned() })
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 10 && t.chars().all(|c| matches!(c, '─' | '━' | '╌' | '-' | '╭' | '╮' | '╰' | '╯'))
}

/// Finds a Claude Code permission prompt ("Do you want to ...?" followed by numbered options).
pub fn find_claude_permission_prompt(screen: &str) -> Option<PermissionPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    let q = lines.iter().rposition(|l| l.contains("Do you want to"))?;
    let mut options = Vec::new();
    for l in &lines[q + 1..] {
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
    if options.len() < 2 {
        return None;
    }
    let start = lines[..q].iter().rposition(|l| is_rule(l)).map(|i| i + 1).unwrap_or(q.saturating_sub(12));
    let start = start.max(q.saturating_sub(20));
    let text = lines[start..=q]
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    Some(PermissionPrompt { text, options })
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
        let p = find_claude_permission_prompt(screen).unwrap();
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[0].label, "Yes");
        assert!(p.options[1].label.ends_with("in C:\\work"));
        assert!(p.text.starts_with("Bash command"));
        assert!(p.text.ends_with("Do you want to proceed?"));
    }

    #[test]
    fn ignores_plain_screen() {
        assert!(find_claude_permission_prompt("❯ hello\n1. not a prompt\n").is_none());
    }
}
