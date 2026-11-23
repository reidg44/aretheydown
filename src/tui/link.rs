//! OSC 8 terminal hyperlinks. After the detail pane is drawn, each cell of an
//! incident URL gets its own complete hyperlink (open, character, close), so
//! any cell (on any wrapped row) opens the full URL, and a partial redraw can
//! never leave a link open. Each cell keeps its real width through
//! `CellDiffOption::ForcedWidth`, because ratatui would otherwise count the
//! escape bytes as columns. Terminals without OSC 8 drop the sequence and show
//! the plain character. Every cell of one URL shares an `id`, so a terminal
//! that groups links by id underlines the whole URL (all its rows) on hover.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroU16;

use ratatui::buffer::{Buffer, CellDiffOption};
use unicode_width::UnicodeWidthStr;

/// Starts an OSC 8 sequence: `params;URI` and [`ST`] follow. An empty URI
/// ends the link.
const OSC8: &str = "\x1b]8;";
/// String Terminator: ends the OSC 8 sequence.
const ST: &str = "\x1b\\";

/// Longest URL to link; longer ones show as plain text. Each cell repeats the
/// URL, so a redraw costs about its length squared in bytes.
const MAX_URL_LEN: usize = 512;

/// Whether `url` can go inside an OSC 8 sequence: printable ASCII with no
/// spaces. A feed URL with an escape or control character would otherwise
/// write raw terminal commands to the screen.
#[must_use]
fn is_linkable(url: &str) -> bool {
    !url.is_empty() && url.len() <= MAX_URL_LEN && url.bytes().all(|b| b.is_ascii_graphic())
}

/// The `id` shared by every cell of `url`'s link: a hash of the URL, so the
/// same URL gets the same id on every draw.
fn link_id(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("ps{:x}", hasher.finish())
}

/// `text` wrapped in an OSC 8 hyperlink to `url` with link `id`, terminated
/// with ST (`ESC \`).
#[must_use]
fn hyperlink(id: &str, url: &str, text: &str) -> String {
    format!("{OSC8}id={id};{url}{ST}{text}{OSC8};{ST}")
}

/// Link the cells of row `y` from column `start` up to (not including) `end`
/// to `url`. Blank cells (the right half of a wide character) are left alone.
/// Does nothing unless [`is_linkable`]`(url)`.
pub fn link_cells(buffer: &mut Buffer, y: u16, start: u16, end: u16, url: &str) {
    if !is_linkable(url) {
        return;
    }
    let id = link_id(url);
    for x in start..end {
        let Some(cell) = buffer.cell_mut((x, y)) else {
            continue;
        };
        let symbol = cell.symbol();
        if symbol.trim().is_empty() {
            continue;
        }
        let width = u16::try_from(symbol.width()).unwrap_or(1);
        let width = NonZeroU16::new(width).unwrap_or(NonZeroU16::MIN);
        let linked = hyperlink(&id, url, symbol);
        cell.set_symbol(&linked)
            .set_diff_option(CellDiffOption::ForcedWidth(width));
    }
}

/// `symbol` with any OSC 8 sequences removed: what a terminal shows.
#[cfg(test)]
#[must_use]
pub fn visible(symbol: &str) -> String {
    let mut out = String::new();
    let mut rest = symbol;
    while let Some(start) = rest.find(OSC8) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find(ST).map_or(after.len(), |i| i + ST.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The URL the cell's symbol links to, if it holds an OSC 8 hyperlink.
#[cfg(test)]
#[must_use]
pub fn target(symbol: &str) -> Option<&str> {
    let start = symbol.find(OSC8)? + OSC8.len();
    let len = symbol[start..].find(ST)?;
    let (_params, url) = symbol[start..start + len].split_once(';')?;
    Some(url).filter(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn hyperlink_wraps_text_in_osc_8() {
        assert_eq!(
            hyperlink("ps1", "https://x.test/a", "h"),
            "\x1b]8;id=ps1;https://x.test/a\x1b\\h\x1b]8;;\x1b\\"
        );
        assert_eq!(visible(&hyperlink("ps1", "https://x.test/a", "h")), "h");
        assert_eq!(
            target(&hyperlink("ps1", "https://x.test/a", "h")),
            Some("https://x.test/a")
        );
        assert_eq!(link_id("https://x.test/a"), link_id("https://x.test/a"));
        assert_ne!(link_id("https://x.test/a"), link_id("https://x.test/b"));
        assert_eq!(visible("plain"), "plain");
        assert_eq!(target("plain"), None);
    }

    #[test]
    fn only_printable_ascii_urls_are_linkable() {
        assert!(is_linkable(
            "https://status.example.com/incidents/e7k?a=1;b=2"
        ));
        assert!(!is_linkable(""));
        assert!(!is_linkable("https://x.test/\x1b]0;pwned\x07"), "escape");
        assert!(!is_linkable("https://x.test/\x07"), "bell");
        assert!(!is_linkable("https://x.test/a b"), "space");
        assert!(!is_linkable("https://bücher.test/"), "non-ASCII");
        assert!(!is_linkable(&format!(
            "https://x.test/{}",
            "a".repeat(MAX_URL_LEN)
        )));
    }

    #[test]
    fn link_cells_links_each_cell_and_keeps_its_width() {
        let mut buffer = Buffer::with_lines(["  ab漢 "]);
        link_cells(&mut buffer, 0, 2, 7, "https://x.test/");
        let cells = &buffer.content;
        assert_eq!(cells[0].symbol(), " ", "before start: untouched");
        for (cell, (text, width)) in cells[2..5].iter().zip([("a", 1), ("b", 1), ("漢", 2)]) {
            assert_eq!(
                cell.symbol(),
                hyperlink(&link_id("https://x.test/"), "https://x.test/", text)
            );
            assert_eq!(
                cell.diff_option,
                CellDiffOption::ForcedWidth(NonZeroU16::new(width).unwrap())
            );
        }
        assert_eq!(cells[5].symbol(), " ", "right half of the wide character");
        assert_eq!(cells[6].symbol(), " ", "blank: not linked");
    }

    #[test]
    fn link_cells_skips_unsafe_urls_and_out_of_range_cells() {
        let mut buffer = Buffer::with_lines(["ab"]);
        link_cells(&mut buffer, 0, 0, 2, "https://x.test/\x1b");
        assert_eq!(buffer, Buffer::with_lines(["ab"]));
        link_cells(&mut buffer, 5, 0, 9, "https://x.test/");
        assert_eq!(buffer.area, Rect::new(0, 0, 2, 1));
        assert_eq!(buffer, Buffer::with_lines(["ab"]));
    }
}
