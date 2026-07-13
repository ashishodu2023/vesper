/// Minimal unified diff for approval UX (no external diff dep).
pub fn unified_diff(path: &str, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");

    // Simple LCS-free line diff: show changed hunks with context=1 via scan
    let max = before_lines.len().max(after_lines.len());
    let mut i = 0usize;
    let mut j = 0usize;
    while i < before_lines.len() || j < after_lines.len() {
        if i < before_lines.len() && j < after_lines.len() && before_lines[i] == after_lines[j] {
            i += 1;
            j += 1;
            continue;
        }
        // collect a hunk
        let hunk_start_i = i;
        let hunk_start_j = j;
        while i < before_lines.len() && (j >= after_lines.len() || before_lines[i] != after_lines[j])
        {
            // advance i until match or end; also try to resync
            if j < after_lines.len() {
                // look ahead for resync
                let mut synced = false;
                for look in 1..=6 {
                    if i + look < before_lines.len()
                        && before_lines[i + look] == after_lines[j]
                    {
                        for k in 0..look {
                            // will emit as deletions below
                            let _ = k;
                        }
                        i += look;
                        synced = true;
                        break;
                    }
                    if j + look < after_lines.len()
                        && i < before_lines.len()
                        && before_lines[i] == after_lines[j + look]
                    {
                        j += look;
                        synced = true;
                        break;
                    }
                }
                if synced {
                    break;
                }
            }
            i += 1;
            if i - hunk_start_i > 40 {
                break;
            }
        }
        while j < after_lines.len()
            && (hunk_start_i >= before_lines.len()
                || i >= before_lines.len()
                || before_lines.get(i) != after_lines.get(j))
        {
            let mut synced = false;
            if i < before_lines.len() {
                for look in 1..=6 {
                    if j + look < after_lines.len() && after_lines[j + look] == before_lines[i] {
                        j += look;
                        synced = true;
                        break;
                    }
                }
            }
            if synced {
                break;
            }
            j += 1;
            if j - hunk_start_j > 40 {
                break;
            }
        }

        let del: Vec<&str> = before_lines[hunk_start_i.min(before_lines.len())..i.min(before_lines.len())].to_vec();
        let add: Vec<&str> = after_lines[hunk_start_j.min(after_lines.len())..j.min(after_lines.len())].to_vec();
        if del.is_empty() && add.is_empty() {
            if i < before_lines.len() {
                i += 1;
            }
            if j < after_lines.len() {
                j += 1;
            }
            continue;
        }
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_start_i + 1,
            del.len().max(1),
            hunk_start_j + 1,
            add.len().max(1)
        ));
        for line in &del {
            out.push_str(&format!("-{line}\n"));
        }
        for line in &add {
            out.push_str(&format!("+{line}\n"));
        }
        if out.lines().count() > 120 {
            out.push_str("...[diff truncated]\n");
            break;
        }
        let _ = max;
    }
    if out.lines().count() <= 2 {
        out.push_str("(no line changes detected, or binary/identical)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_diff() {
        let d = unified_diff("a.txt", "a\nb\nc\n", "a\nB\nc\n");
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
    }
}
