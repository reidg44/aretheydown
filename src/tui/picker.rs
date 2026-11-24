//! The full-screen filter picker `f` opens: a checklist of choices (AWS
//! regions) that can be narrowed by typing. Pure, so it is tested without a
//! terminal; `view::draw_picker` draws it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One choice in the checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What is saved when it is ticked, e.g. `us-east-1`.
    pub key: String,
    /// Shown beside the key, e.g. `US East (N. Virginia)`.
    pub label: String,
    pub ticked: bool,
}

/// What a key press in the picker asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Keep the picker open.
    Stay,
    /// Close it and write nothing.
    Cancel,
    /// Close it and save these keys (the ticked ones, in list order).
    Save(Vec<String>),
    /// Quit the app.
    Quit,
}

/// The checklist's state: its items, the filter, and the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    title: String,
    /// What the items are called, for `No regions match "x"`.
    noun: String,
    items: Vec<Item>,
    /// Shows only items whose key or label holds it (any case).
    filter: String,
    /// Whether keys type into the filter (after `/`, until Enter or Esc).
    typing: bool,
    /// The cursor's position among the items the filter shows.
    cursor: usize,
}

impl Picker {
    /// A picker titled `title` over `items`, with the cursor on the first.
    #[must_use]
    pub fn new(title: &str, noun: &str, items: Vec<Item>) -> Self {
        Self {
            title: title.to_owned(),
            noun: noun.to_owned(),
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
    /// filter or (with none) cancels, and `q` quits. Ctrl-C always quits.
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
            _ if self.typing => self.type_key(key.code),
            KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('/') => self.typing = true,
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

    /// Tick or untick the item under the cursor.
    fn toggle(&mut self) {
        if let Some(&index) = self.visible().get(self.cursor) {
            self.items[index].ticked = !self.items[index].ticked;
        }
    }

    /// The indexes (into [`Picker::items`]) of the items the filter shows,
    /// in list order.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        let filter = self.filter.trim().to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                filter.is_empty()
                    || item.key.to_lowercase().contains(&filter)
                    || item.label.to_lowercase().contains(&filter)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The ticked keys, in list order, filtered out or not.
    #[must_use]
    pub fn picked(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| item.ticked)
            .map(|item| item.key.clone())
            .collect()
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
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
        let item = |key: &str, label: &str, ticked| Item {
            key: key.to_owned(),
            label: label.to_owned(),
            ticked,
        };
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
}
