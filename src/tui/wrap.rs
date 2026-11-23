//! Word-wrap styled lines to a width, with a hanging indent. The detail pane
//! wraps its own lines (rather than using ratatui's `Wrap`) so continuation
//! lines can indent, and so it knows the exact line count for scrolling.

use ratatui::style::Style;
use ratatui::text::{Line, Span, StyledGrapheme};
use unicode_width::UnicodeWidthStr;

/// Word-wrap `line` to `width` columns. Lines after the first start with
/// `indent` spaces, so a wrapped incident title sits under its text, not its
/// bullet. Words longer than a line are split; spaces at a break are dropped.
#[must_use]
pub fn wrap(line: &Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let indent = if indent < width { indent } else { 0 };
    let graphemes: Vec<StyledGrapheme> = line.styled_graphemes(Style::default()).collect();

    let mut rows = Rows::new(indent);
    for chunk in graphemes.chunk_by(|a, b| is_space(a) == is_space(b)) {
        if chunk.iter().all(is_space) {
            // Counted like any text; trimmed off if a break follows.
            for space in chunk {
                rows.push(space.clone());
            }
            continue;
        }
        let word_width: usize = chunk.iter().map(|g| g.symbol.width()).sum();
        if rows.has_word && rows.column + word_width > width {
            rows.break_row();
        }
        for (i, grapheme) in chunk.iter().enumerate() {
            // Split a word wider than the row, but always place one grapheme
            // per row so a very narrow pane still makes progress.
            if (rows.has_word || i > 0) && rows.column + grapheme.symbol.width() > width {
                rows.break_row();
            }
            rows.push(grapheme.clone());
            rows.has_word = true;
        }
    }
    rows.finish()
}

/// The rows built so far (the last is the current one), and where it stands.
struct Rows<'a> {
    built: Vec<Vec<StyledGrapheme<'a>>>,
    /// Columns used on the current row.
    column: usize,
    /// Whether the current row holds a word yet. A word only moves to the next
    /// row when this one already has one; otherwise (e.g. after leading
    /// spaces) it starts here and is split.
    has_word: bool,
    indent: usize,
}

impl<'a> Rows<'a> {
    fn new(indent: usize) -> Self {
        Self {
            built: vec![Vec::new()],
            column: 0,
            has_word: false,
            indent,
        }
    }

    fn current(&mut self) -> &mut Vec<StyledGrapheme<'a>> {
        self.built.last_mut().expect("starts with one row")
    }

    fn push(&mut self, grapheme: StyledGrapheme<'a>) {
        self.column += grapheme.symbol.width();
        self.current().push(grapheme);
    }

    /// End the current row (without its trailing spaces) and start an indented one.
    fn break_row(&mut self) {
        trim_trailing_spaces(self.current());
        self.built.push(vec![
            StyledGrapheme::new(" ", Style::default());
            self.indent
        ]);
        self.column = self.indent;
        self.has_word = false;
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        trim_trailing_spaces(self.current());
        self.built.iter().map(|row| join_graphemes(row)).collect()
    }
}

/// Whether `grapheme` is a plain space, the only place a line may break.
fn is_space(grapheme: &StyledGrapheme) -> bool {
    grapheme.symbol == " "
}

/// Drop the spaces at the end of `row`, so a broken row has no trailing gap.
fn trim_trailing_spaces(row: &mut Vec<StyledGrapheme>) {
    while row.last().is_some_and(is_space) {
        row.pop();
    }
}

/// One line from styled graphemes, merging neighbours of the same style into one span.
fn join_graphemes(row: &[StyledGrapheme]) -> Line<'static> {
    let spans: Vec<Span<'static>> = row
        .chunk_by(|a, b| a.style == b.style)
        .map(|run| {
            let text: String = run.iter().map(|g| g.symbol).collect();
            Span::styled(text, run[0].style)
        })
        .collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Stylize;

    use super::*;

    fn text(line: impl Into<Line<'static>>, width: usize, indent: usize) -> Vec<String> {
        wrap(&line.into(), width, indent)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn breaks_at_spaces_and_indents_continuations() {
        let line = Line::from(vec![Span::from("• "), Span::from("aaa bbb cc").bold()]);
        assert_eq!(text(line.clone(), 6, 2), ["• aaa", "  bbb", "  cc"]);
        assert_eq!(wrap(&line, 6, 2)[1].spans[1].style, Style::new().bold());
    }

    #[test]
    fn a_space_in_the_last_column_still_breaks() {
        assert_eq!(text("aaaaaaaaa b", 10, 0), ["aaaaaaaaa", "b"]);
        assert_eq!(text("aaaaaaaaa  b", 10, 0), ["aaaaaaaaa", "b"]);
    }

    #[test]
    fn splits_words_longer_than_a_row() {
        assert_eq!(
            text("x abcdefghij", 4, 1),
            ["x", " abc", " def", " ghi", " j"]
        );
    }

    #[test]
    fn leading_spaces_stay_on_the_first_row() {
        assert_eq!(text("  abcdefgh", 6, 2), ["  abcd", "  efgh"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(text("ab 漢字漢字", 6, 0), ["ab", "漢字漢", "字"]);
    }

    #[test]
    fn edge_widths_are_safe() {
        assert_eq!(text("", 10, 2), [""], "a blank line stays");
        assert_eq!(
            text("ab cd", 2, 2),
            ["ab", "cd"],
            "indent as wide as the row is dropped"
        );
        assert_eq!(
            text("abc", 0, 0),
            ["a", "b", "c"],
            "zero width acts as one column"
        );
    }
}
