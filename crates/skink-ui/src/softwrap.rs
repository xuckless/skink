//! Soft wrap in the editor view: the setting, the per-tab toggle and
//! mapping screen rows back to text (the row layout is skink-core's
//! [`skink_core::wrap`]).

use crate::App;
use crate::doc::WrapPref;
use skink_core::{VRow, Wrap};

impl App {
    /// How the live document wraps in a text area `width` cells wide.
    /// Rendered previews lay out their own text and never wrap here.
    pub(crate) fn wrap_for(&self, width: usize, page: bool) -> Wrap {
        let s = self.doc_settings();
        let on = match self.doc.wrap {
            WrapPref::Settings => s.soft_wrap,
            WrapPref::On => true,
            WrapPref::Off => false,
        };
        // One cell is kept free for the cursor after a full row.
        let w = width.saturating_sub(1).max(1);
        match (on && !page, s.word_wrap) {
            (false, _) => Wrap::Off,
            (true, true) => Wrap::Words(w),
            (true, false) => Wrap::Chars(w),
        }
    }

    fn wrap_on(&self) -> bool {
        match self.doc.wrap {
            WrapPref::Settings => self.doc_settings().soft_wrap,
            WrapPref::On => true,
            WrapPref::Off => false,
        }
    }

    /// Turn soft wrap on or off for this tab.
    pub(crate) fn set_soft_wrap(&mut self, on: bool) {
        self.doc.wrap = if on { WrapPref::On } else { WrapPref::Off };
        self.doc.editor.message = Some(
            if on {
                "Soft wrap on (this tab; \"softwrap\" in settings for all)"
            } else {
                "Soft wrap off (this tab)"
            }
            .into(),
        );
    }

    /// `ToggleSoftWrap`, `:wrap`
    pub(crate) fn toggle_soft_wrap(&mut self) {
        let on = !self.wrap_on();
        self.set_soft_wrap(on);
    }

    /// The text row under screen row `row` of the editor (sticky headers
    /// first); `None` with no text at all.
    pub(crate) fn vrow_at_row(&self, row: usize, height: usize) -> Option<VRow> {
        let ed = &self.doc.editor;
        let vrows = ed.visual_rows(height);
        let lines: Vec<usize> = vrows.iter().map(|r| r.line).collect();
        let sticky = self.sticky_lines(&lines, height);
        if let Some(&l) = sticky.get(row) {
            return ed.line_rows(l).first().copied();
        }
        vrows.get(row).or(vrows.last()).copied()
    }

    /// Char column at screen column `x` of the text area (gutter removed)
    /// on text row `r`.
    pub(crate) fn col_at(&self, r: &VRow, x: usize) -> usize {
        let ed = &self.doc.editor;
        let text = ed.buffer.line_text(r.line);
        match ed.wrap() {
            Wrap::Off => crate::text::char_col_at(&text, x + self.doc.hview.offset, ed.tab_width),
            Wrap::Words(_) | Wrap::Chars(_) => {
                skink_core::wrap::col_in_row(&text, r, x, ed.tab_width)
            }
        }
    }
}
