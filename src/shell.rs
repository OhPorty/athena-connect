//! Command lines written for bash, made to work in Windows PowerShell (5.1, the one every Windows has).

/// Defined before every command: whether what ran since error number `since` succeeded. A program's exit code is
/// what counts for programs, and a PowerShell command's own error for those; lines a program wrote to its error
/// output (Windows PowerShell turns them into errors under `2>&1`) aren't failures.
pub const POWERSHELL_PRELUDE: &str = "function __AthenaOk($since) { $n = $Error.Count - $since; $bad = 0; if ($n -gt 0) { $bad = @($Error | Select-Object -First $n | Where-Object { $_.FullyQualifiedErrorId -notlike 'NativeCommandError*' }).Count }; return (($global:LASTEXITCODE -eq 0) -and ($bad -eq 0)) }";

/// A command as Windows PowerShell can run it:
/// - `a && b` runs b only if a succeeded, `a || b` only if it failed (Windows PowerShell has neither);
/// - `2>&1` is dropped: the connector collects a command's error output with its output already, and under it
///   Windows PowerShell turns every line a program writes there (like cargo's "Compiling …") into an error.
///
/// Only text outside quotes is changed (and chains only outside brackets and braces); a command with none of
/// this is left exactly as it is.
pub fn for_powershell(command: &str) -> String {
    let mut parts: Vec<(Option<&'static str>, String)> = vec![(None, String::new())];
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    let mut changed = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let current = &mut parts.last_mut().unwrap().1;
        match quote {
            Some(q) => {
                current.push(c);
                if c == '`' && q == '"' {
                    if let Some(n) = next {
                        current.push(n);
                        i += 1;
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    current.push(c);
                }
                '`' => {
                    current.push(c);
                    if let Some(n) = next {
                        current.push(n);
                        i += 1;
                    }
                }
                '(' | '{' | '[' => {
                    depth += 1;
                    current.push(c);
                }
                ')' | '}' | ']' => {
                    depth -= 1;
                    current.push(c);
                }
                '2' if chars[i..].starts_with(&['2', '>', '&', '1']) && (i == 0 || chars[i - 1].is_whitespace()) => {
                    changed = true;
                    i += 3;
                }
                '&' | '|' if depth == 0 && next == Some(c) => {
                    parts.push((Some(if c == '&' { "&&" } else { "||" }), String::new()));
                    changed = true;
                    i += 1;
                }
                _ => current.push(c),
            },
        }
        i += 1;
    }
    if !changed {
        return command.to_string();
    }
    if parts.len() == 1 {
        return parts[0].1.trim_end().to_string();
    }
    let start = "$__athenaSince = $Error.Count; $global:LASTEXITCODE = 0";
    let mut out = format!("{start}\n{}", parts[0].1.trim());
    for (op, part) in &parts[1..] {
        let test = if *op == Some("&&") { "(__AthenaOk $__athenaSince)" } else { "(-not (__AthenaOk $__athenaSince))" };
        out.push_str(&format!("\nif {test} {{\n{start}\n{}", part.trim()));
    }
    out.push_str(&"\n}".repeat(parts.len() - 1));
    out
}

#[cfg(test)]
mod tests {
    use super::for_powershell;

    #[test]
    fn chains_become_conditions() {
        let out = for_powershell("cargo build && cargo test");
        assert!(out.contains("cargo build\nif (__AthenaOk $__athenaSince) {"), "{out}");
        assert!(out.trim_end().ends_with("cargo test\n}"), "{out}");
        let out = for_powershell("a && b || c");
        assert!(out.contains("if (-not (__AthenaOk $__athenaSince)) {"), "{out}");
        assert_eq!(out.matches('{').count(), out.matches('}').count(), "{out}");
    }

    #[test]
    fn drops_error_redirection_outside_quotes() {
        assert_eq!(for_powershell("cargo build 2>&1"), "cargo build");
        assert!(!for_powershell("cargo build 2>&1 && cargo test 2>&1").contains("2>&1"));
        assert_eq!(for_powershell("echo '2>&1'"), "echo '2>&1'");
        assert_eq!(for_powershell("Write-Output x12>&1y"), "Write-Output x12>&1y");
    }

    #[test]
    fn leaves_everything_else_alone() {
        for cmd in ["cargo build; cargo test", "Get-ChildItem | Select-Object Name", "echo 'a && b'", "echo \"x || y\"", "if ($x -and $y) { a }", "& cargo build", "Write-Output (1 -band 3)", "Invoke-Command { a && b }"] {
            assert_eq!(for_powershell(cmd), cmd, "{cmd}");
        }
    }
}
