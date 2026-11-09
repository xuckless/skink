//! Project-wide search: ripgrep's crates (`ignore`, `grep-regex`,
//! `grep-searcher`, MIT/Unlicense) walk the project on a worker thread and
//! stream matches into a results tab; replace goes through the refactor
//! preview.

use crate::doc::Jump;
use crate::refactor::{FilePatch, Preview, Source};
use crate::{App, prompt::PromptKind};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use skink_core::SearchOptions;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Stop after this many matches (the results tab stays usable).
const MAX_HITS: usize = 5_000;
/// Matches per file shown (the count still says how many there are).
const MAX_PER_FILE: usize = 200;
/// Longest preview of a matching line.
const PREVIEW_CHARS: usize = 160;
/// How often streamed results redraw the tab.
const REFRESH: Duration = Duration::from_millis(120);

#[derive(Debug, Clone)]
pub struct Hit {
    /// 0-based line and char column of the first match on the line.
    pub line: usize,
    pub col: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
}

#[derive(Debug)]
pub enum Msg {
    File(FileHits),
    Finished { capped: bool },
    Failed(String),
}

/// What was searched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub opts: SearchOptions,
    pub root: PathBuf,
}

impl Query {
    pub fn title(&self) -> String {
        format!("Search: {}", self.text)
    }
}

/// The project search lifecycle.
#[allow(clippy::large_enum_variant)]
pub enum SearchJob {
    Idle,
    Running {
        query: Query,
        rx: mpsc::Receiver<Msg>,
        /// Dropping it stops the worker.
        _cancel: Cancel,
        found: Vec<FileHits>,
        drawn: Instant,
    },
    Done {
        query: Query,
        found: Vec<FileHits>,
    },
}

/// Stops the worker when the running job is dropped or replaced.
pub struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Walk `root` and send matches. Runs on its own thread.
fn search(
    root: &Path,
    pattern: &str,
    insensitive: bool,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Msg>,
    wake: &dyn Fn(),
) {
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(insensitive)
        .build(pattern)
    {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(Msg::Failed(format!("bad pattern: {e}")));
            return wake();
        }
    };
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let mut total = 0;
    let mut woke = Instant::now();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let mut hits = Vec::new();
        let _ = searcher.search_path(
            &matcher,
            entry.path(),
            UTF8(|lnum, line| {
                use grep_matcher::Matcher;
                let col = matcher
                    .find(line.as_bytes())
                    .ok()
                    .flatten()
                    .map_or(0, |m| line[..m.start()].chars().count());
                let text: String = line
                    .trim_end_matches(['\n', '\r'])
                    .chars()
                    .take(PREVIEW_CHARS)
                    .collect();
                hits.push(Hit {
                    line: lnum.saturating_sub(1) as usize,
                    col,
                    text,
                });
                Ok(hits.len() < MAX_PER_FILE)
            }),
        );
        if hits.is_empty() {
            continue;
        }
        total += hits.len();
        let _ = tx.send(Msg::File(FileHits {
            path: entry.path().to_path_buf(),
            hits,
        }));
        if woke.elapsed() >= REFRESH {
            wake();
            woke = Instant::now();
        }
        if total >= MAX_HITS {
            let _ = tx.send(Msg::Finished { capped: true });
            return wake();
        }
    }
    let _ = tx.send(Msg::Finished { capped: false });
    wake();
}

/// The results tab: a header per file, a line per match, a summary.
fn rows(query: &Query, found: &[FileHits], state: &str) -> Vec<(String, Option<Jump>)> {
    let n: usize = found.iter().map(|f| f.hits.len()).sum();
    let mut out = vec![
        (
            format!(
                "{n} match{} in {} file{} for {:?}{state}",
                if n == 1 { "" } else { "es" },
                found.len(),
                if found.len() == 1 { "" } else { "s" },
                query.text
            ),
            None,
        ),
        (String::new(), None),
    ];
    for f in found {
        let rel = f.path.strip_prefix(&query.root).unwrap_or(&f.path);
        let first = f.hits.first().map(|h| Jump {
            path: f.path.clone(),
            line: h.line,
            col: h.col,
        });
        out.push((format!("{} ({})", rel.display(), f.hits.len()), first));
        for h in &f.hits {
            out.push((
                format!("  {:>5}:{:<3} {}", h.line + 1, h.col + 1, h.text.trim()),
                Some(Jump {
                    path: f.path.clone(),
                    line: h.line,
                    col: h.col,
                }),
            ));
        }
        out.push((String::new(), None));
    }
    out
}

impl App {
    /// Open the project-search bar, pre-filled with the selection.
    pub fn project_search_prompt(&mut self) {
        let seed = self
            .doc
            .editor
            .selected_text()
            .filter(|s| !s.contains('\n'))
            .unwrap_or_default();
        let label = search_label(&self.doc.editor.search_opts);
        self.open_prompt(PromptKind::ProjectSearch, &label, &seed);
    }

    /// Start searching the project for `text` with the find-bar toggles.
    pub fn start_project_search(&mut self, text: &str) {
        let root = self.search_root();
        self.search_in(root, text);
    }

    pub(crate) fn search_in(&mut self, root: PathBuf, text: &str) {
        if text.is_empty() {
            return;
        }
        let query = Query {
            text: text.to_owned(),
            opts: self.doc.editor.search_opts,
            root,
        };
        let (pattern, insensitive) = query.opts.pattern(text);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let wake = self.wake.clone();
        let (root, flag) = (query.root.clone(), cancel.clone());
        let started = std::thread::Builder::new()
            .name("project-search".into())
            .spawn(move || {
                let w = || {
                    if let Some(w) = &wake {
                        w();
                    }
                };
                search(&root, &pattern, insensitive, &flag, &tx, &w);
            });
        if let Err(e) = started {
            self.doc.editor.message = Some(format!("search: {e}"));
            return;
        }
        self.open_results(&query.title(), rows(&query, &[], " (searching…)"));
        self.search = SearchJob::Running {
            query,
            rx,
            _cancel: Cancel(cancel),
            found: Vec::new(),
            drawn: Instant::now(),
        };
    }

    /// Where project search looks: the file tree's root, else the project.
    fn search_root(&self) -> PathBuf {
        self.tree()
            .map(|t| t.root.clone())
            .unwrap_or_else(|| self.project_root())
    }

    /// Take streamed results; refresh the tab now and then and at the end.
    pub(crate) fn poll_search(&mut self) {
        let SearchJob::Running {
            rx, found, drawn, ..
        } = &mut self.search
        else {
            return;
        };
        let mut end = None;
        let mut got = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::File(f)) => {
                    found.push(f);
                    got = true;
                }
                Ok(Msg::Finished { capped }) => {
                    end = Some(Ok(capped));
                    break;
                }
                Ok(Msg::Failed(e)) => {
                    end = Some(Err(e));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    end = Some(Ok(false));
                    break;
                }
            }
        }
        let refresh = got && drawn.elapsed() >= REFRESH;
        if refresh {
            *drawn = Instant::now();
        }
        match end {
            None if refresh => {
                if let SearchJob::Running { query, found, .. } = &self.search {
                    let r = rows(query, found, " (searching…)");
                    let title = query.title();
                    self.refresh_results(&title, r);
                }
            }
            None => {}
            Some(result) => {
                let SearchJob::Running { query, found, .. } =
                    std::mem::replace(&mut self.search, SearchJob::Idle)
                else {
                    return;
                };
                match result {
                    Ok(capped) => {
                        let note = if capped { " (stopped at 5000)" } else { "" };
                        self.refresh_results(&query.title(), rows(&query, &found, note));
                        let n: usize = found.iter().map(|f| f.hits.len()).sum();
                        self.toast(
                            crate::nav::Level::Info,
                            format!("Search done: {n} matches in {} files", found.len()),
                        );
                        self.doc.editor.message =
                            Some("Enter jumps to a match · :greprep WITH replaces them".into());
                        self.search = SearchJob::Done { query, found };
                    }
                    Err(e) => self.toast(crate::nav::Level::Error, e),
                }
            }
        }
    }

    /// Update an open results tab in place, keeping its cursor.
    fn refresh_results(&mut self, title: &str, rows: Vec<(String, Option<Jump>)>) {
        let Some(i) = self.find_doc(|d| {
            d.canon.is_none()
                && d.editor.buffer.is_read_only()
                && d.editor.buffer.title().as_deref() == Some(title)
        }) else {
            return;
        };
        if let Some(d) = self.doc_at_mut(i) {
            let view = d.editor.view();
            let text: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
            d.replace_view(title, &text.join("\n"));
            d.editor.set_view(view);
            d.links = crate::doc::Links::Lines(rows.into_iter().map(|(_, j)| j).collect());
        }
    }

    /// Stop a running search (Esc in its tab, or a new search).
    pub fn stop_project_search(&mut self) {
        if let SearchJob::Running { query, found, .. } =
            std::mem::replace(&mut self.search, SearchJob::Idle)
        {
            self.refresh_results(&query.title(), rows(&query, &found, " (stopped)"));
            self.search = SearchJob::Done { query, found };
            self.doc.editor.message = Some("Search stopped".into());
        }
    }

    /// Replace every match of the last project search with `with`, through
    /// the refactor preview.
    pub fn project_replace(&mut self, with: &str) {
        let SearchJob::Done { query, found, .. } = &self.search else {
            self.doc.editor.message =
                Some("Search first (:grep or Alt-s), then :greprep WITH".into());
            return;
        };
        let re = match query.opts.compile(&query.text) {
            Ok(re) => re,
            Err(e) => {
                self.doc.editor.message = Some(format!("bad regex: {e}"));
                return;
            }
        };
        let expand = query.opts.regex;
        let title = format!("Replace {:?} with {with:?}", query.text);
        let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
        let files: Vec<FilePatch> = paths
            .into_iter()
            .filter_map(|p| {
                let base = self.text_of(&p)?;
                let new = skink_core::replace_in_text(&re, &base, with, expand);
                (new != base).then(|| FilePatch::new(p, base, &new))
            })
            .collect();
        self.open_refactor(Preview::new(&title, Source::Search, files));
    }
}

/// "Search project: " plus the active toggles (same keys as the find bar).
pub fn search_label(o: &SearchOptions) -> String {
    let mut l = String::from("Search project");
    if o.regex {
        l.push_str(" .*");
    }
    if o.case == skink_core::Case::Sensitive {
        l.push_str(" Aa");
    }
    if o.whole_word {
        l.push_str(" \\b");
    }
    l.push_str(": ");
    l
}
