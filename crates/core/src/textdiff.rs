// ---
// relationships:
//   implements: github-release-executor
// ---

//! Unified patches over line-oriented text.
//!
//! A reviewer decides whether to accept a proposed workflow transformation by
//! reading it, so the comparison result carries the same patch a reviewer would
//! apply by hand rather than a summary of what changed.

/// Context lines shown either side of a change.
const CONTEXT: usize = 3;

/// Render a unified patch that turns `before` into `after`.
///
/// An empty result means the two texts are identical.
pub fn unified(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    let from = split(before);
    let to = split(after);
    let hunks = hunks(&common(&from, &to), from.len(), to.len());
    let mut patch = format!("--- a/{path}\n+++ b/{path}\n");
    for hunk in hunks {
        patch.push_str(&render(&hunk, &from, &to));
    }
    patch
}

/// Split text into lines, remembering whether the last line ended with a newline.
fn split(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(index) => {
                lines.push(Line {
                    content: &rest[..index],
                    terminated: true,
                });
                rest = &rest[index + 1..];
            }
            None => {
                lines.push(Line {
                    content: rest,
                    terminated: false,
                });
                rest = "";
            }
        }
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line<'a> {
    content: &'a str,
    terminated: bool,
}

/// Longest common subsequence of two line sequences, as index pairs.
fn common(from: &[Line<'_>], to: &[Line<'_>]) -> Vec<(usize, usize)> {
    let mut lengths = vec![vec![0usize; to.len() + 1]; from.len() + 1];
    for (row, left) in from.iter().enumerate().rev() {
        for (column, right) in to.iter().enumerate().rev() {
            lengths[row][column] = if left == right {
                lengths[row + 1][column + 1] + 1
            } else {
                lengths[row + 1][column].max(lengths[row][column + 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut row, mut column) = (0, 0);
    while row < from.len() && column < to.len() {
        if from[row] == to[column] {
            pairs.push((row, column));
            row += 1;
            column += 1;
        } else if lengths[row + 1][column] >= lengths[row][column + 1] {
            row += 1;
        } else {
            column += 1;
        }
    }
    pairs
}

#[derive(Debug)]
struct Hunk {
    from: std::ops::Range<usize>,
    to: std::ops::Range<usize>,
}

/// Group differing regions into hunks padded with context.
fn hunks(pairs: &[(usize, usize)], from_len: usize, to_len: usize) -> Vec<Hunk> {
    let mut changes = Vec::new();
    let (mut row, mut column) = (0, 0);
    for &(next_row, next_column) in pairs {
        if next_row > row || next_column > column {
            changes.push((row..next_row, column..next_column));
        }
        row = next_row + 1;
        column = next_column + 1;
    }
    if row < from_len || column < to_len {
        changes.push((row..from_len, column..to_len));
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    for (from, to) in changes {
        let start_from = from.start.saturating_sub(CONTEXT);
        let start_to = to.start.saturating_sub(CONTEXT);
        let end_from = (from.end + CONTEXT).min(from_len);
        let end_to = (to.end + CONTEXT).min(to_len);
        match hunks.last_mut() {
            Some(last) if last.from.end >= start_from && last.to.end >= start_to => {
                last.from.end = end_from;
                last.to.end = end_to;
            }
            _ => hunks.push(Hunk {
                from: start_from..end_from,
                to: start_to..end_to,
            }),
        }
    }
    hunks
}

fn render(hunk: &Hunk, from: &[Line<'_>], to: &[Line<'_>]) -> String {
    let mut body = String::new();
    let pairs = common(&from[hunk.from.clone()], &to[hunk.to.clone()]);
    let (mut row, mut column) = (hunk.from.start, hunk.to.start);
    let emit = |marker: char, line: &Line<'_>, body: &mut String| {
        body.push(marker);
        body.push_str(line.content);
        body.push('\n');
        if !line.terminated {
            body.push_str("\\ No newline at end of file\n");
        }
    };
    for (offset_row, offset_column) in pairs {
        let (next_row, next_column) = (hunk.from.start + offset_row, hunk.to.start + offset_column);
        while row < next_row {
            emit('-', &from[row], &mut body);
            row += 1;
        }
        while column < next_column {
            emit('+', &to[column], &mut body);
            column += 1;
        }
        emit(' ', &from[row], &mut body);
        row += 1;
        column += 1;
    }
    while row < hunk.from.end {
        emit('-', &from[row], &mut body);
        row += 1;
    }
    while column < hunk.to.end {
        emit('+', &to[column], &mut body);
        column += 1;
    }
    format!(
        "@@ -{},{} +{},{} @@\n{body}",
        hunk.from.start + 1,
        hunk.from.len(),
        hunk.to.start + 1,
        hunk.to.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_no_patch_for_identical_text() {
        assert_eq!(unified("file.yml", "one\ntwo\n", "one\ntwo\n"), "");
    }

    #[test]
    fn renders_a_replacement_with_surrounding_context() {
        let patch = unified(
            ".github/workflows/release.yml",
            "a\nb\nc\nd\ne\nf\ng\nh\n",
            "a\nb\nc\nD\ne\nf\ng\nh\n",
        );
        assert_eq!(
            patch,
            "--- a/.github/workflows/release.yml\n+++ b/.github/workflows/release.yml\n@@ -1,7 +1,7 @@\n a\n b\n c\n-d\n+D\n e\n f\n g\n"
        );
    }

    #[test]
    fn renders_insertions_and_deletions_in_one_hunk() {
        let patch = unified("file.yml", "one\ntwo\n", "one\ninserted\ntwo\n");
        assert!(patch.contains("+inserted"), "{patch}");
        assert!(patch.contains("@@ -1,2 +1,3 @@"), "{patch}");
    }

    #[test]
    fn marks_a_missing_final_newline() {
        let patch = unified("file.yml", "one\ntwo", "one\ntwo\n");
        assert!(patch.contains("\\ No newline at end of file"), "{patch}");
    }

    #[test]
    fn separates_distant_changes_into_distinct_hunks() {
        let before = (0..40).map(|n| format!("line{n}\n")).collect::<String>();
        let after = before
            .replace("line1\n", "changed1\n")
            .replace("line35\n", "changed35\n");
        let patch = unified("file.yml", &before, &after);
        assert_eq!(patch.matches("@@ ").count(), 2, "{patch}");
    }
}
