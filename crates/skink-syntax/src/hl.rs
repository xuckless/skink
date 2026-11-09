//! Highlight classes and the capture-name mapping.

/// A syntax highlight class. Themes map these to colours/styles.
///
/// Classes are derived from tree-sitter capture names (`@keyword.control`,
/// `@function.method`, ...). The **most specific known prefix wins**: the
/// capture name is split on `.` and progressively shortened until a name in the
/// table below is found (so `@keyword.control.repeat` resolves via `keyword`).
///
/// | capture name(s)                                               | class         |
/// |---------------------------------------------------------------|---------------|
/// | `keyword` (and every `keyword.*`)                             | `Keyword`     |
/// | `function`, `function.call`, `function.special`               | `Function`    |
/// | `function.method`, `method`                                   | `Method`      |
/// | `function.macro`, `macro`                                     | `Macro`       |
/// | `function.builtin`, `type.builtin`, `variable.builtin`        | `Builtin`     |
/// | `type`, `type.*` (other)                                      | `Type`        |
/// | `constructor`                                                 | `Constructor` |
/// | `constant`, `constant.builtin`                                | `Constant`    |
/// | `variable`                                                    | `Variable`    |
/// | `variable.parameter`, `parameter`                             | `Parameter`   |
/// | `property`, `field`, `variable.member`, `string.special.key`  | `Property`    |
/// | `string`, `string.*`, `character`                             | `String`      |
/// | `escape`, `string.escape`, `string.special.escape`            | `Escape`      |
/// | `number`, `number.*`, `float`                                 | `Number`      |
/// | `comment`, `comment.*`                                        | `Comment`     |
/// | `operator`                                                    | `Operator`    |
/// | `punctuation`, `punctuation.*`                                | `Punctuation` |
/// | `attribute`                                                   | `Attribute`   |
/// | `tag`, `tag.*`                                                | `Tag`         |
/// | `label`                                                       | `Label`       |
/// | `namespace`, `module`                                         | `Namespace`   |
/// | `boolean`                                                     | `Boolean`     |
/// | `error`                                                       | `Error`       |
/// | `text.title`, `markup.heading`                                | `Heading`     |
/// | `text.emphasis`, `markup.italic`                              | `Emphasis`    |
/// | `text.strong`, `markup.strong`                                | `Strong`      |
/// | `text.uri`, `text.reference`, `markup.link`                   | `Link`        |
/// | `text.literal`, `markup.raw`                                  | `Literal`     |
///
/// Everything else (`@spell`, `@none`, `@embedded`, helper captures such as
/// `@_name` or `@import` used only by predicates) is ignored and paints nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Hl {
    Keyword,
    Function,
    Method,
    Macro,
    Type,
    Constructor,
    Constant,
    Variable,
    Parameter,
    Property,
    String,
    Escape,
    Number,
    Comment,
    Operator,
    Punctuation,
    Attribute,
    Tag,
    Label,
    Namespace,
    Boolean,
    /// Built-in functions, types and variables (`println`, `i32`, `self`).
    Builtin,
    Error,
    /// Markup: headings, emphasis, strong text, links, inline code.
    Heading,
    Emphasis,
    Strong,
    Link,
    Literal,
}

/// Maps a capture name (without the leading `@`) to a class, or `None` if the
/// capture should be ignored. Most specific known prefix wins.
pub(crate) fn from_capture(name: &str) -> Option<Hl> {
    if name.starts_with('_') {
        return None;
    }
    let mut n = name;
    loop {
        if let Some(hl) = exact(n) {
            return hl;
        }
        n = &n[..n.rfind('.')?];
    }
}

/// `Some(Some(hl))` known, `Some(None)` known-and-ignored, `None` unknown.
fn exact(n: &str) -> Option<Option<Hl>> {
    use Hl::*;
    Some(Some(match n {
        "keyword" => Keyword,
        "function" | "function.call" | "function.special" => Function,
        "function.method" | "function.method.call" | "method" => Method,
        "function.macro" | "macro" => Macro,
        "function.builtin" | "type.builtin" | "variable.builtin" => Builtin,
        "type" => Type,
        "constructor" => Constructor,
        "constant" | "constant.builtin" => Constant,
        "variable" => Variable,
        "variable.parameter" | "parameter" => Parameter,
        "property" | "field" | "variable.member" | "string.special.key" => Property,
        "string" | "character" => String,
        "escape" | "string.escape" | "string.special.escape" => Escape,
        "number" | "float" => Number,
        "comment" => Comment,
        "operator" => Operator,
        "punctuation" => Punctuation,
        "attribute" => Attribute,
        "tag" => Tag,
        "label" => Label,
        "namespace" | "module" => Namespace,
        "boolean" => Boolean,
        "error" => Error,
        "text.title" | "markup.heading" => Heading,
        "text.emphasis" | "markup.italic" => Emphasis,
        "text.strong" | "markup.strong" | "markup.bold" => Strong,
        "text.uri" | "text.reference" | "markup.link" => Link,
        "text.literal" | "markup.raw" => Literal,
        "markup.list" => Punctuation,
        "markup.quote" => Comment,
        "spell" | "nospell" | "none" | "embedded" | "conceal" | "injection" => return Some(None),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_resolution() {
        assert_eq!(from_capture("keyword.control.repeat"), Some(Hl::Keyword));
        assert_eq!(from_capture("function.method"), Some(Hl::Method));
        assert_eq!(from_capture("function.method.call"), Some(Hl::Method));
        assert_eq!(from_capture("function.builtin"), Some(Hl::Builtin));
        assert_eq!(from_capture("type.builtin"), Some(Hl::Builtin));
        assert_eq!(from_capture("type.definition"), Some(Hl::Type));
        assert_eq!(from_capture("string.escape"), Some(Hl::Escape));
        assert_eq!(from_capture("string.special.key"), Some(Hl::Property));
        assert_eq!(from_capture("string.special"), Some(Hl::String));
        assert_eq!(from_capture("punctuation.bracket"), Some(Hl::Punctuation));
        assert_eq!(from_capture("number.float"), Some(Hl::Number));
        assert_eq!(from_capture("comment.documentation"), Some(Hl::Comment));
        assert_eq!(from_capture("spell"), None);
        assert_eq!(from_capture("import"), None);
        assert_eq!(from_capture("_private"), None);
        assert_eq!(from_capture("embedded"), None);
    }
}
