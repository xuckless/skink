//! "Ask" (a free-form answer about the code) and "implement" (new code
//! inserted at the cursor) requests, the two prompt-box modes besides
//! rewrite and search. Templates are original (99 has no license; only its
//! idea of one prompt box with several modes is reused).

use std::path::PathBuf;

use super::context::{
    ContextConfig, Diagnostic, FileSnapshot, ProjectFiles, assemble, render_context,
};
use super::coords::{CharPos, CharRange, TextIndex, uses_crlf};
use super::edit::EditProposal;
use super::rewrite::{ExtractError, extract_code};
use super::{Prompt, RequestKind};
use crate::agent::markdown_fence;

const ASK_CONTRACT: &str = "You are answering a question from a developer inside their code editor. \
Answer concisely in Markdown. Refer to code by file and line. Do not modify any files.";

const IMPLEMENT_CONTRACT: &str = "You are writing new code that the editor inserts at the \
<CURSOR> marker of the file below. Reply with ONLY the code to insert, in one fenced code block: \
no surrounding unchanged code, no marker. Explanations the developer asks for go inside the code as \
comments in the file's language, never as prose outside the block. Match the file's style and indentation.";

/// A question about the current file (and selection, when there is one).
pub fn build_ask(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    file: &FileSnapshot,
    selection: Option<CharRange>,
    diagnostics: &[Diagnostic],
    question: &str,
) -> Prompt {
    let a = assemble(cfg, files, file, selection, true, diagnostics, question);
    let lang = a.language.unwrap_or("");
    let mut t = String::from(ASK_CONTRACT);
    t.push_str("\n\n");
    render_context(&mut t, cfg, &a);
    match &a.selection {
        Some(sel) => t.push_str(&format!(
            "## Selected code ({}, lines {}-{})\n{}\n\n",
            a.file,
            sel.first_line,
            sel.last_line,
            markdown_fence(lang, &sel.text)
        )),
        None => t.push_str(&format!("## Current file\n{}\n\n", a.file)),
    }
    t.push_str(&format!("## QUESTION\n{}\n", question.trim()));
    Prompt {
        kind: RequestKind::Worker,
        text: t,
    }
}

/// A prepared "insert new code here" request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInsert {
    pub prompt: Prompt,
    pub path: PathBuf,
    /// Where the code goes: the cursor's line when it is blank (replaced),
    /// else an empty range at the start of the next line.
    pub region: CharRange,
    pub original: String,
}

pub fn build_implement(
    cfg: &ContextConfig,
    files: &dyn ProjectFiles,
    file: &FileSnapshot,
    cursor: CharPos,
    diagnostics: &[Diagnostic],
    instruction: &str,
) -> BuiltInsert {
    let idx = TextIndex::new(&file.text);
    let line = cursor.line.min(idx.line_count().saturating_sub(1));
    let text = idx.line(line).unwrap_or("");
    let blank = text.trim().is_empty();
    let region = if blank {
        idx.expand_to_lines(CharRange::new(CharPos::new(line, 0), CharPos::new(line, 0)))
    } else {
        let at = CharPos::new(line + 1, 0);
        CharRange::new(at, at)
    };
    let original = idx.slice(region).to_owned();
    // Show the model the file around the insertion point, marked.
    let a = assemble(
        cfg,
        files,
        file,
        Some(CharRange::new(CharPos::new(line, 0), CharPos::new(line, 0))),
        true,
        diagnostics,
        instruction,
    );
    let lang = a.language.unwrap_or("");
    let lines: Vec<&str> = file.text.split_inclusive('\n').collect();
    let from = line.saturating_sub(cfg.lines_before);
    let to = (line + 1 + cfg.lines_after).min(lines.len());
    let mut around = String::new();
    for (i, l) in lines.iter().enumerate().take(to).skip(from) {
        if i == line && blank {
            around.push_str("<CURSOR>\n");
            continue;
        }
        around.push_str(l);
        if i == line {
            if !l.ends_with('\n') {
                around.push('\n');
            }
            around.push_str("<CURSOR>\n");
        }
    }
    let mut t = String::from(IMPLEMENT_CONTRACT);
    t.push_str("\n\n");
    render_context(&mut t, cfg, &a);
    t.push_str(&format!(
        "## File\n{}, insertion at line {}\n{}\n\n",
        a.file,
        line + 1,
        markdown_fence(lang, &around)
    ));
    t.push_str(&format!("## INSTRUCTION\n{}\n", instruction.trim()));
    BuiltInsert {
        prompt: Prompt {
            kind: RequestKind::RewriteSelection,
            text: t,
        },
        path: file.path.clone(),
        region,
        original,
    }
}

impl BuiltInsert {
    /// The agent's answer as an insertion. Always ends with a line break so
    /// the following line stays on its own line.
    pub fn finish(&self, reply: &str, file_text: &str) -> Result<EditProposal, ExtractError> {
        let ex = extract_code(reply);
        if ex.code.trim().is_empty() {
            return Err(ExtractError::Empty);
        }
        let nl = if uses_crlf(file_text) { "\r\n" } else { "\n" };
        let mut code = ex.code.replace("\r\n", "\n");
        if nl == "\r\n" {
            code = code.replace('\n', "\r\n");
        }
        if !code.ends_with('\n') {
            code.push_str(nl);
        }
        // Inserting after a last line without a newline: start a new line.
        let idx = TextIndex::new(file_text);
        if self.original.is_empty()
            && self.region.start.line >= idx.line_count()
            && !file_text.is_empty()
            && !file_text.ends_with('\n')
        {
            code.insert_str(0, nl);
        }
        Ok(EditProposal::new(
            self.path.clone(),
            self.region,
            self.original.clone(),
            code,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompter::context::DiskFiles;

    fn snap(text: &str) -> FileSnapshot {
        FileSnapshot {
            path: PathBuf::from("/nowhere/a.rs"),
            text: text.into(),
        }
    }

    #[test]
    fn implement_inserts_after_the_cursor_line_or_fills_a_blank_one() {
        let cfg = ContextConfig::new("/nowhere");
        let text = "fn a() {}\n\nfn b() {}\n";
        // Blank line 1: replaced by the code.
        let b = build_implement(
            &cfg,
            &DiskFiles,
            &snap(text),
            CharPos::new(1, 0),
            &[],
            "add c",
        );
        assert_eq!(b.original, "\n");
        assert!(b.prompt.text.contains("<CURSOR>"));
        let e = b.finish("```rust\nfn c() {}\n```", text).unwrap();
        assert_eq!(
            e.accept_all_to(text).unwrap(),
            "fn a() {}\nfn c() {}\nfn b() {}\n"
        );
        // Non-blank line 0: inserted below it.
        let b = build_implement(&cfg, &DiskFiles, &snap(text), CharPos::new(0, 3), &[], "x");
        let e = b.finish("fn c() {}", text).unwrap();
        assert_eq!(
            e.accept_all_to(text).unwrap(),
            "fn a() {}\nfn c() {}\n\nfn b() {}\n"
        );
        // Last line without a newline.
        let t2 = "fn a() {}";
        let b = build_implement(&cfg, &DiskFiles, &snap(t2), CharPos::new(0, 0), &[], "x");
        let e = b.finish("fn c() {}", t2).unwrap();
        assert_eq!(e.accept_all_to(t2).unwrap(), "fn a() {}\nfn c() {}\n");
    }
}
