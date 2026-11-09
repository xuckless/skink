//! Live previews of the active file in a tab of their own: rendered Markdown
//! (headings, emphasis, lists, task lists, quotes, tables, rules, links and
//! syntax-coloured code fences) and SVG drawn with half-block cells.
//!
//! A preview is a read-only document whose text is the rendered output; its
//! colours come from the renderer instead of tree-sitter. Images inside
//! Markdown are shown as their alt text.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use skink_syntax::{Hl, Span};
use std::path::PathBuf;

/// What a preview tab shows, and whether it is up to date.
pub struct Preview {
    /// The file previewed (canonical path).
    pub source: PathBuf,
    pub kind: PreviewKind,
    pub state: RenderState,
}

pub enum PreviewKind {
    Markdown(Rendered),
    Svg(SvgImage),
}

/// Whether the rendered output matches the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderState {
    /// Needs rendering (new, source edited, or resized).
    Stale,
    /// Rendered from source revision `rev` at `width` columns.
    Fresh { rev: u64, width: u16 },
}

/// Rendered Markdown: the text, colour spans per line, links and where each
/// source block landed (for scroll-sync).
#[derive(Debug, Default, Clone)]
pub struct Rendered {
    pub text: String,
    pub spans: Vec<Vec<Span>>,
    pub links: Vec<LinkSpan>,
    /// (source line, preview line) per block, in order.
    pub line_map: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub url: String,
}

impl Rendered {
    /// Preview line showing source line `src` (the block at or before it).
    pub fn preview_line(&self, src: usize) -> usize {
        self.line_map
            .iter()
            .take_while(|(s, _)| *s <= src)
            .last()
            .map_or(0, |(_, p)| *p)
    }

    /// The link under (line, char column), if any.
    pub fn link_at(&self, line: usize, col: usize) -> Option<&str> {
        self.links
            .iter()
            .find(|l| l.line == line && (l.start..l.end).contains(&col))
            .map(|l| l.url.as_str())
    }
}

/// One output line being built.
#[derive(Default)]
struct OutLine {
    text: String,
    len: usize,
    spans: Vec<Span>,
}

impl OutLine {
    fn push(&mut self, s: &str, hl: Option<Hl>) {
        let n = s.chars().count();
        if let Some(hl) = hl
            && n > 0
        {
            self.spans.push(Span {
                start: self.len,
                end: self.len + n,
                hl,
            });
        }
        self.text.push_str(s);
        self.len += n;
    }
}

/// A styled stretch of inline text.
#[derive(Clone)]
struct Run {
    text: String,
    hl: Option<Hl>,
    url: Option<String>,
}

/// Container context: block quotes and list items add a prefix per line.
#[derive(Clone)]
enum Frame {
    Quote,
    /// A list; `next` is the next ordered number, `None` for bullets.
    List {
        next: Option<u64>,
    },
    /// An item: its marker is printed on its first line only.
    Item {
        marker: Option<String>,
    },
}

struct Renderer {
    width: usize,
    out: Vec<OutLine>,
    links: Vec<LinkSpan>,
    line_map: Vec<(usize, usize)>,
    frames: Vec<Frame>,
    runs: Vec<Run>,
    /// Inline style stack (emphasis, strong, links, headings).
    styles: Vec<Hl>,
    link: Option<String>,
    code: Option<(String, String)>,
    table: Option<Table>,
    line_starts: Vec<usize>,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    cell: String,
    head_rows: usize,
}

/// Render Markdown `src` for a view `width` columns wide.
pub fn render_markdown(src: &str, width: u16) -> Rendered {
    let width = (width as usize).clamp(20, 120);
    let mut line_starts = vec![0];
    line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
    let mut r = Renderer {
        width,
        out: Vec::new(),
        links: Vec::new(),
        line_map: Vec::new(),
        frames: Vec::new(),
        runs: Vec::new(),
        styles: Vec::new(),
        link: None,
        code: None,
        table: None,
        line_starts,
    };
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        r.event(ev, range.start);
    }
    r.flush();
    let mut text = String::new();
    let mut spans = Vec::with_capacity(r.out.len());
    for (i, l) in r.out.into_iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(l.text.trim_end());
        spans.push(l.spans);
    }
    Rendered {
        text,
        spans,
        links: r.links,
        line_map: r.line_map,
    }
}

impl Renderer {
    fn src_line(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= byte) - 1
    }

    fn style(&self) -> Option<Hl> {
        self.styles.last().copied()
    }

    /// The per-line prefix from quotes and lists; `first` prints item markers.
    fn prefix(&mut self, first: bool, out: &mut OutLine) {
        let n = self.frames.len();
        for i in 0..n {
            match &mut self.frames[i] {
                Frame::Quote => out.push("▌ ", Some(Hl::Comment)),
                Frame::List { .. } => {}
                Frame::Item { marker } => match marker.take() {
                    Some(m) if first => out.push(&m, Some(Hl::Punctuation)),
                    Some(m) => {
                        out.push(&" ".repeat(m.chars().count()), None);
                        *marker = Some(m);
                    }
                    None => out.push(&" ".repeat(2), None),
                },
            }
        }
    }

    fn blank(&mut self) {
        let is_blank = |l: &OutLine| {
            l.text
                .trim_matches(|c: char| c == '▌' || c.is_whitespace())
                .is_empty()
        };
        if self.out.last().is_some_and(|l| !is_blank(l)) {
            let mut l = OutLine::default();
            for f in &self.frames {
                if matches!(f, Frame::Quote) {
                    l.push("▌", Some(Hl::Comment));
                }
            }
            self.out.push(l);
        }
    }

    /// Word-wrap the pending inline runs into lines.
    fn flush(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.runs);
        let mut line = OutLine::default();
        self.prefix(true, &mut line);
        let indent = line.len;
        let mut at_start = true;
        for run in runs {
            // Split into words, keeping single spaces as separators.
            for (wi, word) in run.text.split(' ').enumerate() {
                if wi > 0 && !at_start {
                    if line.len + 1 > self.width {
                        self.out.push(std::mem::take(&mut line));
                        self.prefix(false, &mut line);
                        at_start = true;
                    } else {
                        line.push(" ", run.hl);
                    }
                }
                if word.is_empty() {
                    continue;
                }
                let w = word.chars().count();
                if !at_start && line.len + w > self.width && line.len > indent {
                    self.out.push(std::mem::take(&mut line));
                    self.prefix(false, &mut line);
                }
                let start = line.len;
                line.push(word, run.hl);
                if let Some(url) = &run.url {
                    self.links.push(LinkSpan {
                        line: self.out.len(),
                        start,
                        end: line.len,
                        url: url.clone(),
                    });
                }
                at_start = false;
            }
        }
        self.out.push(line);
    }

    fn text(&mut self, s: &str) {
        if let Some((_, buf)) = &mut self.code {
            buf.push_str(s);
            return;
        }
        if let Some(t) = &mut self.table {
            t.cell.push_str(s);
            return;
        }
        self.runs.push(Run {
            text: s.replace('\n', " "),
            hl: self.style(),
            url: self.link.clone(),
        });
    }

    fn event(&mut self, ev: Event, at: usize) {
        match ev {
            Event::Start(tag) => self.start(tag, at),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => {
                if let Some(tb) = &mut self.table {
                    tb.cell.push_str(&t);
                } else {
                    self.runs.push(Run {
                        text: t.to_string(),
                        hl: Some(Hl::Literal),
                        url: self.link.clone(),
                    });
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank();
                let mut l = OutLine::default();
                self.prefix(true, &mut l);
                let w = self.width.saturating_sub(l.len);
                l.push(&"─".repeat(w), Some(Hl::Punctuation));
                self.out.push(l);
                self.blank();
            }
            Event::TaskListMarker(done) => self.runs.push(Run {
                text: if done { "☑ " } else { "☐ " }.into(),
                hl: Some(if done { Hl::String } else { Hl::Punctuation }),
                url: None,
            }),
            Event::Html(h) | Event::InlineHtml(h) => self.runs.push(Run {
                text: h.trim_end().replace('\n', " "),
                hl: Some(Hl::Comment),
                url: None,
            }),
            Event::FootnoteReference(f) => self.runs.push(Run {
                text: format!("[{f}]"),
                hl: Some(Hl::Link),
                url: None,
            }),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag, at: usize) {
        let src = self.src_line(at);
        match tag {
            Tag::Paragraph => {
                if self.frames.is_empty() {
                    self.line_map.push((src, self.out.len()));
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                self.line_map.push((src, self.out.len()));
                let hashes = match level {
                    HeadingLevel::H1 => "",
                    HeadingLevel::H2 => "",
                    HeadingLevel::H3 => "### ",
                    HeadingLevel::H4 => "#### ",
                    HeadingLevel::H5 => "##### ",
                    HeadingLevel::H6 => "###### ",
                };
                if !hashes.is_empty() {
                    self.runs.push(Run {
                        text: hashes.trim_end().into(),
                        hl: Some(Hl::Punctuation),
                        url: None,
                    });
                    self.runs.push(Run {
                        text: " ".into(),
                        hl: None,
                        url: None,
                    });
                }
                self.styles.push(Hl::Heading);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.line_map.push((src, self.out.len()));
                self.frames.push(Frame::Quote);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.line_map.push((src, self.out.len()));
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.split([' ', ',']).next().unwrap_or("").to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(first) => {
                self.flush();
                if self.frames.is_empty() {
                    self.line_map.push((src, self.out.len()));
                }
                self.frames.push(Frame::List { next: first });
            }
            Tag::Item => {
                self.flush();
                let marker = match self.frames.iter_mut().rev().find_map(|f| match f {
                    Frame::List { next } => Some(next),
                    _ => None,
                }) {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_owned(),
                };
                self.frames.push(Frame::Item {
                    marker: Some(marker),
                });
            }
            Tag::Emphasis => self.styles.push(Hl::Emphasis),
            Tag::Strong => self.styles.push(Hl::Strong),
            Tag::Strikethrough => self.styles.push(Hl::Comment),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.styles.push(Hl::Link);
            }
            Tag::Image { .. } => {
                // Images are not drawn yet: show the alt text.
                self.runs.push(Run {
                    text: "[image: ".into(),
                    hl: Some(Hl::Comment),
                    url: None,
                });
                self.styles.push(Hl::Comment);
            }
            Tag::Table(_) => {
                self.flush();
                self.line_map.push((src, self.out.len()));
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(t) = &mut self.table {
                    t.cell.clear();
                }
            }
            Tag::MetadataBlock(_) => self.code = Some(("yaml".into(), String::new())),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if !self.in_tight_item() {
                    self.blank();
                }
            }
            TagEnd::Heading(level) => {
                self.styles.pop();
                let start = self.out.len();
                self.flush();
                if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    let w = self.out[start..]
                        .iter()
                        .map(|l| l.len)
                        .max()
                        .unwrap_or(0)
                        .max(3);
                    let mut l = OutLine::default();
                    let ch = if level == HeadingLevel::H1 {
                        "═"
                    } else {
                        "─"
                    };
                    l.push(&ch.repeat(w), Some(Hl::Heading));
                    self.out.push(l);
                }
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.frames.pop();
                // The quote's own trailing "▌" line becomes a plain blank one.
                if self.out.last().is_some_and(|l| l.text.trim() == "▌") {
                    self.out.pop();
                }
                self.blank();
            }
            TagEnd::CodeBlock | TagEnd::MetadataBlock(_) => {
                if let Some((lang, body)) = self.code.take() {
                    self.code_block(&lang, &body);
                }
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.frames.pop();
                if !self.frames.iter().any(|f| matches!(f, Frame::Item { .. })) {
                    self.blank();
                }
            }
            TagEnd::Item => {
                self.flush();
                self.frames.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                self.link = None;
            }
            TagEnd::Image => {
                self.styles.pop();
                self.runs.push(Run {
                    text: "]".into(),
                    hl: Some(Hl::Comment),
                    url: None,
                });
            }
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    let cell = std::mem::take(&mut t.cell);
                    if let Some(row) = t.rows.last_mut() {
                        row.push(cell.trim().to_owned());
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.head_rows = t.rows.len();
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.table(t);
                }
                self.blank();
            }
            _ => {}
        }
    }

    /// Items of a tight list hold their text directly; no blank between them.
    fn in_tight_item(&self) -> bool {
        matches!(self.frames.last(), Some(Frame::Item { .. }))
    }

    fn code_block(&mut self, lang: &str, body: &str) {
        let body = body.strip_suffix('\n').unwrap_or(body);
        let spans: Vec<Vec<Span>> = skink_syntax::Lang::from_name(lang)
            .map(|l| {
                let mut h = skink_syntax::Highlighter::new_inline(l);
                h.update(body);
                h.highlights(body, 0, body.lines().count().max(1))
            })
            .unwrap_or_default();
        for (i, text) in body.split('\n').enumerate() {
            let mut l = OutLine::default();
            self.prefix(false, &mut l);
            l.push("  ", None);
            let off = l.len;
            l.push(&text.replace('\t', "    "), None);
            if !text.contains('\t') {
                l.spans
                    .extend(spans.get(i).into_iter().flatten().map(|s| Span {
                        start: s.start + off,
                        end: s.end + off,
                        hl: s.hl,
                    }));
            }
            self.out.push(l);
        }
    }

    fn table(&mut self, t: Table) {
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![1; cols];
        for row in &t.rows {
            for (i, c) in row.iter().enumerate() {
                widths[i] = widths[i].max(c.chars().count());
            }
        }
        // Squeeze the widest columns until the table fits.
        let frame = 3 * cols + 1;
        while widths.iter().sum::<usize>() + frame > self.width {
            let (i, w) = widths
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|(_, w)| *w)
                .expect("cols > 0");
            if w <= 4 {
                break;
            }
            widths[i] = w - 1;
        }
        let rule = |l: &str, m: &str, r: &str| {
            let mut s = String::from(l);
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if i + 1 == cols { r } else { m });
            }
            s
        };
        let push_rule = |me: &mut Self, s: String| {
            let mut l = OutLine::default();
            me.prefix(false, &mut l);
            l.push(&s, Some(Hl::Punctuation));
            me.out.push(l);
        };
        push_rule(self, rule("┌", "┬", "┐"));
        for (ri, row) in t.rows.iter().enumerate() {
            let mut l = OutLine::default();
            self.prefix(false, &mut l);
            l.push("│", Some(Hl::Punctuation));
            for (i, w) in widths.iter().enumerate() {
                let cell = row.get(i).map_or("", String::as_str);
                let mut shown: String = cell.chars().take(*w).collect();
                if cell.chars().count() > *w && *w > 1 {
                    shown = cell.chars().take(w - 1).collect::<String>() + "…";
                }
                let pad = w - shown.chars().count();
                l.push(" ", None);
                let hl = (ri < t.head_rows).then_some(Hl::Strong);
                l.push(&shown, hl);
                l.push(&" ".repeat(pad + 1), None);
                l.push("│", Some(Hl::Punctuation));
            }
            self.out.push(l);
            if ri + 1 == t.head_rows {
                push_rule(self, rule("├", "┼", "┤"));
            }
        }
        push_rule(self, rule("└", "┴", "┘"));
    }
}

/// An SVG drawn for the terminal: two pixels per cell (`▀` with the top
/// pixel as foreground and the bottom one as background).
#[derive(Debug, Clone)]
pub struct SvgImage {
    /// 1.0 fits the view; +/- zoom in steps.
    pub zoom: f32,
    pub view: SvgView,
}

/// The rasterized cells, or why there are none.
#[derive(Debug, Clone)]
pub enum SvgView {
    /// Rows of cells: (top, bottom) RGBA pixels.
    Cells(Vec<Vec<([u8; 4], [u8; 4])>>),
    Failed(String),
}

impl SvgImage {
    pub fn new() -> Self {
        SvgImage {
            zoom: 1.0,
            view: SvgView::Cells(Vec::new()),
        }
    }
}

/// Rasterize `data` (SVG source) into `cols` x `rows` cells at `zoom`.
#[cfg(feature = "svg-preview")]
pub fn rasterize_svg(data: &[u8], cols: u16, rows: u16, zoom: f32) -> SvgView {
    use resvg::{tiny_skia, usvg};
    let tree = match usvg::Tree::from_data(data, &usvg::Options::default()) {
        Ok(t) => t,
        Err(e) => return SvgView::Failed(format!("not a valid SVG: {e}")),
    };
    let (pw, ph) = (cols.max(1) as u32, rows.max(1) as u32 * 2);
    let size = tree.size();
    let fit = (pw as f32 / size.width()).min(ph as f32 / size.height()) * zoom;
    let (w, h) = (
        ((size.width() * fit).ceil() as u32).clamp(1, 4096),
        ((size.height() * fit).ceil() as u32).clamp(1, 4096),
    );
    let Some(mut pm) = tiny_skia::Pixmap::new(w, h) else {
        return SvgView::Failed("image too large".into());
    };
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(fit, fit),
        &mut pm.as_mut(),
    );
    let px = |x: u32, y: u32| -> [u8; 4] {
        if x >= w || y >= h {
            return [0; 4];
        }
        let c = pm.pixel(x, y).map(|p| p.demultiply()).expect("in bounds");
        [c.red(), c.green(), c.blue(), c.alpha()]
    };
    let cells = (0..h.div_ceil(2))
        .map(|r| (0..w).map(|x| (px(x, r * 2), px(x, r * 2 + 1))).collect())
        .collect();
    SvgView::Cells(cells)
}

#[cfg(not(feature = "svg-preview"))]
pub fn rasterize_svg(_: &[u8], _: u16, _: u16, _: f32) -> SvgView {
    SvgView::Failed("this build has no SVG preview (cargo feature `svg-preview`)".into())
}

/// Draw an SVG preview centered in `area`, blending transparency over a
/// checkerboard-free plain background.
pub fn draw_svg(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    img: &SvgImage,
    theme: &crate::theme::Theme,
) {
    use ratatui::style::{Color, Style};
    let bg = theme.bg;
    let blend = |p: [u8; 4]| {
        let a = p[3] as u16;
        let mix = |c: u8, b: u8| ((c as u16 * a + b as u16 * (255 - a)) / 255) as u8;
        Color::Rgb(mix(p[0], bg.0), mix(p[1], bg.1), mix(p[2], bg.2))
    };
    match &img.view {
        SvgView::Failed(msg) => {
            let y = area.y + area.height / 2;
            let x = area.x + area.width.saturating_sub(msg.chars().count() as u16) / 2;
            buf.set_stringn(
                x,
                y,
                msg,
                area.width as usize,
                Style::new().fg(theme.c(theme.comment)),
            );
        }
        SvgView::Cells(rows) => {
            let h = rows.len() as u16;
            let w = rows.first().map_or(0, Vec::len) as u16;
            let ox = area.x + area.width.saturating_sub(w) / 2;
            let oy = area.y + area.height.saturating_sub(h) / 2;
            for (ry, row) in rows.iter().enumerate().take(area.height as usize) {
                for (rx, (top, bottom)) in row.iter().enumerate().take(area.width as usize) {
                    let (x, y) = (ox + rx as u16, oy + ry as u16);
                    if x < area.right() && y < area.bottom() {
                        buf[(x, y)]
                            .set_char('▀')
                            .set_fg(blend(*top))
                            .set_bg(blend(*bottom));
                    }
                }
            }
        }
    }
}

/// What can be previewed, by file extension.
fn kind_for(path: &std::path::Path) -> Option<PreviewKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" | "mkd" => Some(PreviewKind::Markdown(Rendered::default())),
        "svg" => Some(PreviewKind::Svg(SvgImage::new())),
        _ => None,
    }
}

use crate::{App, doc::DocKind};

impl App {
    /// `:preview`: open (or select) the live preview of the current Markdown
    /// or SVG file; from a preview, go back to its source.
    pub(crate) fn toggle_preview(&mut self) {
        if let DocKind::Preview(p) = &self.doc.kind {
            let src = p.source.clone();
            match self.find_doc(|d| d.canon.as_ref() == Some(&src)) {
                Some(i) => self.switch_to(i),
                None => self.open_path(&src),
            }
            return;
        }
        let Some(src) = self.doc.canon.clone() else {
            self.doc.editor.message = Some("Preview needs a Markdown or SVG file".into());
            return;
        };
        let Some(kind) = kind_for(&src) else {
            self.doc.editor.message = Some("Preview works for Markdown (.md) and SVG files".into());
            return;
        };
        let existing = self.find_doc(|d| matches!(&d.kind, DocKind::Preview(p) if p.source == src));
        if let Some(i) = existing {
            return self.switch_to(i);
        }
        // Scroll-sync starts from the source's cursor line.
        let src_line = self.doc.editor.cursor.line;
        let name = src
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let title = format!("Preview: {name}");
        let idx = self.add_doc(
            skink_core::Editor::new(skink_core::Buffer::view(&title, "")),
            false,
        );
        if let Some(d) = self.doc_at_mut(idx) {
            d.kind = DocKind::Preview(Box::new(Preview {
                source: src,
                kind,
                state: RenderState::Stale,
            }));
            d.syntax = None;
        }
        self.preview_sync = PreviewSync::ToSourceLine(src_line);
    }

    /// Re-render the active preview when its source changed or the view was
    /// resized. Called before each frame; cheap when nothing changed.
    pub(crate) fn refresh_preview(&mut self, width: u16, height: u16) {
        let DocKind::Preview(p) = &self.doc.kind else {
            return;
        };
        let src = p.source.clone();
        // The source text: the open tab (live edits), else the file on disk.
        let (text, rev) = match self.find_doc(|d| d.canon.as_ref() == Some(&src)) {
            Some(i) => {
                let d = self.doc_at(i).expect("found");
                (d.editor.buffer.text(), d.editor.buffer.revision())
            }
            None => match &p.state {
                RenderState::Fresh { rev, width: w } if *w == width => (String::new(), *rev),
                _ => (std::fs::read_to_string(&src).unwrap_or_default(), 0),
            },
        };
        if p.state == (RenderState::Fresh { rev, width }) {
            return;
        }
        let title = self.doc.editor.buffer.title().unwrap_or_default();
        let DocKind::Preview(p) = &mut self.doc.kind else {
            return;
        };
        match &mut p.kind {
            PreviewKind::Markdown(r) => {
                *r = render_markdown(&text, width.saturating_sub(8));
                let (line, scroll) = (self.doc.editor.cursor.line, self.doc.editor.scroll);
                let rendered = r.text.clone();
                let target = match self.preview_sync {
                    PreviewSync::ToSourceLine(l) => Some(r.preview_line(l)),
                    PreviewSync::Done => None,
                };
                self.doc.replace_view(&title, &rendered);
                let ed = &mut self.doc.editor;
                ed.set_viewport_height(height as usize);
                match target {
                    Some(t) => {
                        ed.goto(Some(t + 1), Some(1));
                        ed.scroll = t;
                    }
                    None => {
                        ed.goto(Some(line + 1), Some(1));
                        ed.scroll = scroll;
                    }
                }
            }
            PreviewKind::Svg(img) => {
                let data = std::fs::read(&src).unwrap_or_else(|_| text.into_bytes());
                img.view = rasterize_svg(&data, width, height, img.zoom);
            }
        }
        if let DocKind::Preview(p) = &mut self.doc.kind {
            p.state = RenderState::Fresh { rev, width };
        }
        self.preview_sync = PreviewSync::Done;
    }

    /// Rendered Markdown of the active tab, if it is a Markdown preview.
    pub(crate) fn markdown_preview(&self) -> Option<&Rendered> {
        match &self.doc.kind {
            DocKind::Preview(p) => match &p.kind {
                PreviewKind::Markdown(r) => Some(r),
                PreviewKind::Svg(_) => None,
            },
            DocKind::Text | DocKind::Git(_) | DocKind::Debug(_) => None,
        }
    }

    /// The active tab's SVG image, if it is an SVG preview.
    pub(crate) fn svg_preview(&self) -> Option<&SvgImage> {
        match &self.doc.kind {
            DocKind::Preview(p) => match &p.kind {
                PreviewKind::Svg(img) => Some(img),
                PreviewKind::Markdown(_) => None,
            },
            DocKind::Text | DocKind::Git(_) | DocKind::Debug(_) => None,
        }
    }

    /// Keys a preview handles itself: Enter follows the link under the
    /// cursor (Markdown); `+` / `-` / `0` zoom an SVG. True when consumed.
    pub(crate) fn preview_key(&mut self, key: &skink_config::KeySpec) -> bool {
        use skink_config::Key;
        let DocKind::Preview(p) = &mut self.doc.kind else {
            return false;
        };
        match (&mut p.kind, key.key) {
            (PreviewKind::Markdown(r), Key::Enter) => {
                let (line, col) = (self.doc.editor.cursor.line, self.doc.editor.cursor.col);
                let Some(url) = r.link_at(line, col).map(str::to_owned) else {
                    return false;
                };
                let base = p.source.parent().map(std::path::Path::to_path_buf);
                self.open_link(&url, base.as_deref());
                true
            }
            (PreviewKind::Svg(img), Key::Char(c @ ('+' | '=' | '-' | '0'))) => {
                img.zoom = match c {
                    '0' => 1.0,
                    '-' => (img.zoom / 1.25).max(0.1),
                    _ => (img.zoom * 1.25).min(16.0),
                };
                p.state = RenderState::Stale;
                true
            }
            _ => false,
        }
    }

    /// Open a link: web links in the system browser, relative paths as files.
    fn open_link(&mut self, url: &str, base: Option<&std::path::Path>) {
        let is_web = url.contains("://") || url.starts_with("mailto:");
        if !is_web {
            let path = url.split('#').next().unwrap_or(url);
            if path.is_empty() {
                return;
            }
            let p = base.map_or_else(|| PathBuf::from(path), |b| b.join(path));
            return self.open_path(&p);
        }
        let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
            ("open", vec![url])
        } else if cfg!(windows) {
            ("cmd", vec!["/C", "start", "", url])
        } else {
            ("xdg-open", vec![url])
        };
        let spawned = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.doc.editor.message = Some(match spawned {
            Ok(_) => format!("Opened {url}"),
            Err(e) => format!("Could not open {url}: {e}"),
        });
    }
}

/// Where a newly opened preview should scroll on its first render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewSync {
    #[default]
    Done,
    /// Show the block holding this source line.
    ToSourceLine(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(r: &Rendered) -> Vec<&str> {
        r.text.split('\n').collect()
    }

    #[test]
    fn headings_lists_tasks_quotes_rules() {
        let r = render_markdown(
            "# Title\n\nSome *em* and **strong** text.\n\n- one\n- [x] done\n\n1. a\n2. b\n\n> quoted\n\n---\n",
            60,
        );
        let l = lines(&r);
        assert_eq!(l[0], "Title");
        assert!(l[1].starts_with("═════"));
        assert!(l.contains(&"• one"), "{l:?}");
        assert!(l.contains(&"• ☑ done"), "{l:?}");
        assert!(l.contains(&"2. b"), "{l:?}");
        assert!(l.contains(&"▌ quoted"), "{l:?}");
        assert!(l.iter().any(|s| s.starts_with("────")));
        // Emphasis is styled, not marked up.
        let i = l.iter().position(|s| s.starts_with("Some em")).unwrap();
        assert!(r.spans[i].iter().any(|s| s.hl == Hl::Emphasis));
        assert!(r.spans[0].iter().any(|s| s.hl == Hl::Heading));
    }

    #[test]
    fn code_fences_are_highlighted_and_links_recorded() {
        let r = render_markdown(
            "See [the docs](https://example.com) now.\n\n```rust\nfn main() {}\n```\n",
            60,
        );
        let l = lines(&r);
        let code = l.iter().position(|s| s.trim() == "fn main() {}").unwrap();
        assert!(r.spans[code].iter().any(|s| s.hl == Hl::Keyword));
        let link = &r.links[0];
        assert_eq!(link.url, "https://example.com");
        assert_eq!(
            r.link_at(link.line, link.start),
            Some("https://example.com")
        );
        assert_eq!(&l[link.line][link.start..link.end], "the");
    }

    #[test]
    fn tables_get_box_borders_and_paragraphs_wrap() {
        let r = render_markdown("| a | bb |\n|---|---|\n| 1 | 2 |\n\n", 60);
        let l = lines(&r);
        assert_eq!(l[0], "┌───┬────┐");
        assert_eq!(l[1], "│ a │ bb │");
        assert_eq!(l[2], "├───┼────┤");
        let long = "word ".repeat(40);
        let r = render_markdown(&long, 30);
        assert!(r.text.lines().all(|s| s.chars().count() <= 30));
        assert!(r.text.lines().count() > 3);
    }

    #[test]
    fn scroll_sync_maps_source_lines_to_blocks() {
        let r = render_markdown("# A\n\npara\n\n# B\n\nmore\n", 40);
        let b = r.preview_line(4);
        assert_eq!(lines(&r)[b], "B");
        assert_eq!(r.preview_line(0), 0);
    }
}
