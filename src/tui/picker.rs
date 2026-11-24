//! The full-screen filter picker `f` opens: a checklist of choices (AWS
//! regions, or a Statuspage platform's components under their groups) that
//! can be narrowed by typing. Pure, so it is tested without a terminal;
//! `view::draw_picker` draws it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One choice in the checklist: a top-level item, a heading (a group that
/// can be ticked whole or expanded), or an item under a heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What is saved when it is ticked, e.g. `us-east-1`.
    pub key: String,
    /// Shown beside the key and matched by the filter, e.g. `US East (N. Virginia)`.
    pub label: String,
    /// Shown after the label but not matched, e.g. `not in feed`.
    pub hint: String,
    pub ticked: bool,
    /// Whether it is a heading, with the items that follow it (and name it
    /// as their `parent`) under it.
    pub heading: bool,
    /// The heading it sits under: an index into the picker's items.
    pub parent: Option<usize>,
}

impl Item {
    /// A top-level item with no hint.
    #[must_use]
    pub fn new(key: &str, label: &str, ticked: bool) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            hint: String::new(),
            ticked,
            heading: false,
            parent: None,
        }
    }
}

/// What a key press in the picker asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Keep the picker open.
    Stay,
    /// Close it and write nothing.
    Cancel,
    /// Close it and save these keys ([`Picker::picked`]).
    Save(Vec<String>),
    /// Quit the app.
    Quit,
}

/// The checklist's state: its items, the filter, the cursor, and which
/// headings are expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    title: String,
    /// What the items are called, for `No regions match "x"`.
    noun: String,
    items: Vec<Item>,
    /// Each item's key and label in lower case, for the filter.
    search: Vec<String>,
    /// Per item: whether its children show (headings only).
    expanded: Vec<bool>,
    /// Shows only items whose key or label holds it (any case).
    filter: String,
    /// Whether keys type into the filter (after `/`, until Enter or Esc).
    typing: bool,
    /// The cursor's position among the items the filter shows.
    cursor: usize,
}

impl Picker {
    /// A picker titled `title` over `items` (children right after their
    /// heading), every heading collapsed, with the cursor on the first.
    #[must_use]
    pub fn new(title: &str, noun: &str, items: Vec<Item>) -> Self {
        let search = items
            .iter()
            .map(|item| format!("{}\n{}", item.key, item.label).to_lowercase())
            .collect();
        Self {
            title: title.to_owned(),
            noun: noun.to_owned(),
            expanded: vec![false; items.len()],
            search,
            items,
            filter: String::new(),
            typing: false,
            cursor: 0,
        }
    }

    /// React to a key press. While typing a filter, letters, digits, and
    /// spaces go into it, Backspace deletes, Enter stops typing, and Esc
    /// clears it; the arrow keys move either way. Otherwise ↑/↓ and `j`/`k`
    /// move, Space ticks, `/` starts typing, Enter saves, Esc clears the
    /// filter or (with none) cancels, and `q` quits. On a heading, → (or
    /// Enter, while it is collapsed) expands it and ← collapses it; ← under
    /// a heading collapses it and moves there. Ctrl-C always quits.
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        if key.kind != KeyEventKind::Press {
            return Outcome::Stay;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                Outcome::Quit
            } else {
                Outcome::Stay
            };
        }
        match key.code {
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Right => self.expand(),
            KeyCode::Left => self.collapse(),
            _ if self.typing => self.type_key(key.code),
            KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Enter if self.on_collapsed_heading() => self.expand(),
            KeyCode::Enter => return Outcome::Save(self.picked()),
            KeyCode::Esc if !self.filter.is_empty() => self.set_filter(String::new()),
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('q') => return Outcome::Quit,
            _ => {}
        }
        Outcome::Stay
    }

    /// A key while typing into the filter.
    fn type_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.typing = false,
            KeyCode::Esc => {
                self.typing = false;
                self.set_filter(String::new());
            }
            KeyCode::Backspace => {
                let mut filter = self.filter.clone();
                filter.pop();
                self.set_filter(filter);
            }
            KeyCode::Char(c) if !c.is_control() => {
                let mut filter = self.filter.clone();
                filter.push(c);
                self.set_filter(filter);
            }
            _ => {}
        }
    }

    /// Change the filter; the cursor goes back to the first match.
    fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.cursor = 0;
    }

    fn move_cursor(&mut self, step: isize) {
        let last = self.visible().len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(step).min(last);
    }

    /// The item under the cursor, as an index into [`Picker::items`].
    fn current(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }

    /// Put the cursor on item `index`, if it shows.
    fn focus(&mut self, index: usize) {
        if let Some(row) = self.visible().iter().position(|&i| i == index) {
            self.cursor = row;
        }
    }

    /// Whether the cursor is on a collapsed heading that expanding would
    /// show more of, where Enter expands instead of saving. While a filter
    /// is set, only a heading whose own name matches shows more: one shown
    /// for a matching item under it already shows every item that matches.
    #[must_use]
    pub fn on_collapsed_heading(&self) -> bool {
        let filter = self.filter.trim().to_lowercase();
        self.current().is_some_and(|index| {
            self.items[index].heading
                && !self.expanded[index]
                && (filter.is_empty() || self.search[index].contains(&filter))
        })
    }

    /// Expand the heading under the cursor.
    fn expand(&mut self) {
        if let Some(index) = self.current().filter(|&index| self.items[index].heading) {
            self.expanded[index] = true;
        }
    }

    /// Collapse the heading under the cursor, or the one the cursor's item
    /// sits under (and move there).
    fn collapse(&mut self) {
        let Some(index) = self.current() else { return };
        let heading = if self.items[index].heading {
            index
        } else if let Some(parent) = self.items[index].parent {
            parent
        } else {
            return;
        };
        self.expanded[heading] = false;
        self.focus(heading);
    }

    /// Tick or untick the item under the cursor. An item whose heading is
    /// ticked is covered by it: unticking it unticks the heading and ticks
    /// every other item under it instead.
    fn toggle(&mut self) {
        let Some(index) = self.current() else { return };
        if let Some(parent) = self.items[index].parent.filter(|&p| self.items[p].ticked) {
            self.items[parent].ticked = false;
            for item in self
                .items
                .iter_mut()
                .filter(|item| item.parent == Some(parent))
            {
                item.ticked = true;
            }
            self.items[index].ticked = false;
        } else {
            self.items[index].ticked = !self.items[index].ticked;
        }
    }

    /// Whether item `index` sits under a ticked heading, so it counts
    /// whatever its own tick says.
    #[must_use]
    pub fn covered(&self, index: usize) -> bool {
        self.items[index]
            .parent
            .is_some_and(|parent| self.items[parent].ticked)
    }

    /// Whether heading `index` shows its items.
    #[must_use]
    pub fn expanded(&self, index: usize) -> bool {
        self.expanded[index]
    }

    /// The indexes (into [`Picker::items`]) of the items on screen, in list
    /// order. With no filter: every top-level item, and the items under
    /// expanded headings. With one: the items whose key or label holds it
    /// (any case), each matching item's heading, and every item under a
    /// matching heading that is expanded.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        let filter = self.filter.trim().to_lowercase();
        let matches = |index: usize| filter.is_empty() || self.search[index].contains(&filter);
        let child_matches = |heading: usize| {
            self.items
                .iter()
                .enumerate()
                .any(|(i, item)| item.parent == Some(heading) && matches(i))
        };
        (0..self.items.len())
            .filter(|&index| match self.items[index].parent {
                None if self.items[index].heading => {
                    matches(index) || (!filter.is_empty() && child_matches(index))
                }
                None => matches(index),
                Some(parent) if filter.is_empty() => self.expanded[parent],
                Some(parent) => {
                    self.search[index].contains(&filter)
                        || (self.expanded[parent] && self.search[parent].contains(&filter))
                }
            })
            .collect()
    }

    /// The ticked keys, in list order, filtered out or not, leaving out
    /// items a ticked heading covers and keys already listed (any case,
    /// trimmed).
    #[must_use]
    pub fn picked(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.items
            .iter()
            .enumerate()
            .filter(|&(index, item)| item.ticked && !self.covered(index))
            .map(|(_, item)| item.key.clone())
            .filter(|key| seen.insert(crate::model::name_key(key)))
            .collect()
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Whether any item is a heading (so the footer explains ←/→).
    #[must_use]
    pub fn has_headings(&self) -> bool {
        self.items.iter().any(|item| item.heading)
    }

    /// The cursor's position among [`Picker::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn noun(&self) -> &str {
        &self.noun
    }

    /// The filter text, as typed.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Whether keys go into the filter.
    #[must_use]
    pub fn typing(&self) -> bool {
        self.typing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker() -> Picker {
        let item = Item::new;
        Picker::new(
            "AWS regions",
            "regions",
            vec![
                item("us-east-1", "US East (N. Virginia)", false),
                item("us-west-2", "US West (Oregon)", true),
                item("eu-west-1", "Europe (Ireland)", false),
            ],
        )
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Outcome {
        picker.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            press(picker, KeyCode::Char(c));
        }
    }

    #[test]
    fn arrows_and_j_k_move_within_the_list() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Up);
        assert_eq!(picker.cursor(), 0);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char('j'));
        press(&mut picker, KeyCode::Char('j'));
        assert_eq!(picker.cursor(), 2, "stops at the last item");
        press(&mut picker, KeyCode::Char('k'));
        assert_eq!(picker.cursor(), 1);
    }

    #[test]
    fn space_toggles_and_enter_saves_the_ticked_keys_in_list_order() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Down);
        assert_eq!(press(&mut picker, KeyCode::Char(' ')), Outcome::Stay);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Up);
        press(&mut picker, KeyCode::Up);
        press(&mut picker, KeyCode::Char(' '));
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["us-east-1".to_owned(), "eu-west-1".to_owned()])
        );
    }

    #[test]
    fn esc_cancels_and_q_or_ctrl_c_quits() {
        let mut picker = picker();
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Cancel);
        assert_eq!(press(&mut picker, KeyCode::Char('q')), Outcome::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(picker.key(ctrl_c), Outcome::Quit);
    }

    #[test]
    fn slash_filters_by_key_or_label_in_any_case() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('/'));
        assert!(picker.typing());
        type_text(&mut picker, "WEST");
        assert_eq!(picker.visible(), [1, 2]);
        press(&mut picker, KeyCode::Backspace);
        type_text(&mut picker, "t-2");
        assert_eq!(picker.visible(), [1]);
        press(&mut picker, KeyCode::Esc);
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "ireland");
        assert_eq!(picker.visible(), [2]);
        type_text(&mut picker, "x");
        assert_eq!(picker.visible(), Vec::<usize>::new());
    }

    #[test]
    fn while_typing_j_k_q_and_space_are_text() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('/'));
        for c in ['j', 'k', 'q', ' '] {
            assert_eq!(press(&mut picker, KeyCode::Char(c)), Outcome::Stay);
        }
        assert_eq!(picker.filter(), "jkq ");
        assert!(
            picker.items().iter().filter(|item| item.ticked).count() == 1,
            "space did not tick"
        );
    }

    #[test]
    fn enter_ends_typing_and_keeps_the_filter_for_ticking() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "europe");
        assert_eq!(press(&mut picker, KeyCode::Enter), Outcome::Stay);
        assert!(!picker.typing());
        assert_eq!(picker.filter(), "europe");
        press(&mut picker, KeyCode::Char(' '));
        assert!(picker.items()[2].ticked, "ticks the filtered item");
        // The filter only hides: us-west-2 is still saved.
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["us-west-2".to_owned(), "eu-west-1".to_owned()])
        );
    }

    #[test]
    fn esc_clears_the_filter_before_it_cancels() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "eu");
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Stay);
        assert_eq!(picker.filter(), "");
        assert!(!picker.typing());
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "eu");
        press(&mut picker, KeyCode::Enter);
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Stay);
        assert_eq!(picker.visible().len(), 3);
        assert_eq!(press(&mut picker, KeyCode::Esc), Outcome::Cancel);
    }

    #[test]
    fn a_filter_change_puts_the_cursor_on_the_first_match() {
        let mut picker = picker();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "us");
        assert_eq!(picker.cursor(), 0);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.cursor(), 1, "the arrows move while typing");
    }

    /// `Old` (not in feed, ticked), `API` (Requests, Webhooks), `Pages`,
    /// `Infra` (DNS, API Gateway).
    fn tree() -> Picker {
        let top = |key: &str, ticked| Item::new(key, "", ticked);
        let heading = |key: &str| Item {
            heading: true,
            ..top(key, false)
        };
        let under = |key: &str, parent| Item {
            parent: Some(parent),
            ..top(key, false)
        };
        Picker::new(
            "GitHub components",
            "components",
            vec![
                top("Old", true),
                heading("API"),
                under("Requests", 1),
                under("Webhooks", 1),
                top("Pages", false),
                heading("Infra"),
                under("DNS", 5),
                under("API Gateway", 5),
            ],
        )
    }

    #[test]
    fn headings_start_collapsed_and_expand_with_right_or_enter() {
        let mut picker = tree();
        assert_eq!(picker.visible(), [0, 1, 4, 5]);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Right);
        assert_eq!(picker.visible(), [0, 1, 2, 3, 4, 5]);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Stay,
            "Enter on a collapsed heading expands it"
        );
        assert_eq!(picker.visible(), [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["Old".to_owned()]),
            "Enter on an expanded heading saves"
        );
    }

    #[test]
    fn left_collapses_a_heading_from_it_or_from_under_it() {
        let mut picker = tree();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Right);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.cursor(), 3, "on Webhooks");
        press(&mut picker, KeyCode::Left);
        assert_eq!(picker.visible(), [0, 1, 4, 5]);
        assert_eq!(picker.cursor(), 1, "back on API");
        press(&mut picker, KeyCode::Left);
        assert_eq!(picker.cursor(), 1, "a collapsed heading stays put");
        press(&mut picker, KeyCode::Up);
        press(&mut picker, KeyCode::Left);
        press(&mut picker, KeyCode::Right);
        assert_eq!(picker.cursor(), 0, "←/→ do nothing on a top-level item");
    }

    #[test]
    fn a_ticked_heading_covers_its_items_and_saves_its_own_name() {
        let mut picker = tree();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Char(' '));
        press(&mut picker, KeyCode::Right);
        press(&mut picker, KeyCode::Down);
        assert!(picker.covered(2) && picker.covered(3));
        assert!(!picker.covered(6));
        assert_eq!(picker.picked(), ["Old", "API"]);

        // Unticking a covered item ticks the rest of the group instead.
        press(&mut picker, KeyCode::Char(' '));
        assert!(!picker.items()[1].ticked);
        assert_eq!(picker.picked(), ["Old", "Webhooks"]);
    }

    #[test]
    fn single_items_under_a_heading_can_be_ticked() {
        let mut picker = tree();
        for code in [
            KeyCode::Up,
            KeyCode::Char(' '),
            KeyCode::Down,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Char(' '),
        ] {
            press(&mut picker, code);
        }
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["Requests".to_owned()])
        );
    }

    #[test]
    fn the_filter_shows_matching_items_with_their_heading() {
        let mut picker = tree();
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "dns");
        assert_eq!(picker.visible(), [5, 6], "collapsed Infra shows for DNS");
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        type_text(&mut picker, "api");
        assert_eq!(
            picker.visible(),
            [1, 5, 7],
            "API matches by name, Infra through API Gateway"
        );
        // An expanded matching heading shows all its items.
        press(&mut picker, KeyCode::Right);
        assert_eq!(picker.visible(), [1, 2, 3, 5, 7]);
        type_text(&mut picker, "x");
        assert_eq!(picker.visible(), Vec::<usize>::new());
    }

    #[test]
    fn enter_saves_on_a_heading_shown_only_for_a_match_under_it() {
        let mut picker = tree();
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "dns");
        press(&mut picker, KeyCode::Enter);
        assert_eq!(picker.visible(), [5, 6]);
        assert!(!picker.on_collapsed_heading(), "DNS already shows");
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["Old".to_owned()])
        );
    }

    #[test]
    fn a_name_listed_twice_is_saved_once() {
        let mut picker = Picker::new(
            "t",
            "components",
            vec![Item::new("API", "", true), Item::new(" api ", "", true)],
        );
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Outcome::Save(vec!["API".to_owned()])
        );
    }

    #[test]
    fn the_filter_stays_quick_over_a_thousand_items() {
        let mut items = Vec::new();
        for group in 0..20 {
            let heading = items.len();
            items.push(Item {
                heading: true,
                ..Item::new(&format!("Region {group}"), "", false)
            });
            for n in 0..50 {
                items.push(Item {
                    parent: Some(heading),
                    ..Item::new(&format!("Datacenter {group}-{n} - (X{n:02})"), "", false)
                });
            }
        }
        let mut picker = Picker::new("Cloudflare components", "components", items);
        let started = std::time::Instant::now();
        press(&mut picker, KeyCode::Char('/'));
        type_text(&mut picker, "datacenter 1");
        for _ in 0..20 {
            press(&mut picker, KeyCode::Down);
        }
        assert_ne!(picker.visible(), Vec::<usize>::new());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
