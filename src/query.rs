//! Toplevel query language, syntax-compatible with cosmic-ext-window-helper.
//!
//! ```text
//! app_id = 'firefox'
//! title ~= 'work|documents'i
//! output.name != 'HDMI-1'
//! workspace.is_visible and not (is_active or app_id = 'com.system76.CosmicTerm')
//! ```

use regex::RegexBuilder;

use crate::i18n;

use crate::state::{
    ST_ACTIVATED, ST_FULLSCREEN, ST_MAXIMIZED, ST_MINIMIZED, ST_STICKY, State, Window,
};

#[derive(Debug)]
pub struct QueryError(pub String);

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::fill1(i18n::m().query_error, &self.0))
    }
}

impl std::error::Error for QueryError {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Re,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum StrField {
    Id,
    AppId,
    Title,
    WorkspaceName,
    OutputName,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BoolField {
    IsActive,
    IsActiveAppId,
    IsMaximized,
    IsMinimized,
    IsFullscreen,
    IsSticky,
    WorkspaceIsVisible,
    WorkspaceHasFocus,
    OutputHasFocus,
}

#[derive(Debug)]
enum Node {
    Str {
        neg: bool,
        field: StrField,
        op: Op,
        value: String,
        ci: bool,
    },
    Bool {
        neg: bool,
        field: BoolField,
    },
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Word(String),
    /// Quoted string, plus whether it carried the `i` suffix.
    Quoted(String, bool),
}

fn tokenize(input: &str) -> Result<Vec<Tok>, QueryError> {
    let mut out = Vec::new();
    let mut it = input.chars().peekable();
    while let Some(&c) = it.peek() {
        match c {
            c if c.is_whitespace() => {
                it.next();
            }
            '(' => {
                it.next();
                out.push(Tok::LParen);
            }
            ')' => {
                it.next();
                out.push(Tok::RParen);
            }
            // Operators may appear without surrounding spaces, e.g. `app_id='x'`
            '=' => {
                it.next();
                out.push(Tok::Word("=".into()));
            }
            '!' => {
                it.next();
                if it.peek() == Some(&'=') {
                    it.next();
                    out.push(Tok::Word("!=".into()));
                } else {
                    out.push(Tok::Word("!".into()));
                }
            }
            '~' => {
                it.next();
                if it.peek() == Some(&'=') {
                    it.next();
                    out.push(Tok::Word("~=".into()));
                } else {
                    out.push(Tok::Word("~".into()));
                }
            }
            '\'' | '"' => {
                let quote = c;
                it.next();
                let mut s = String::new();
                let mut closed = false;
                while let Some(ch) = it.next() {
                    if ch == '\\' {
                        if let Some(&n) = it.peek() {
                            s.push(n);
                            it.next();
                        }
                    } else if ch == quote {
                        closed = true;
                        break;
                    } else {
                        s.push(ch);
                    }
                }
                if !closed {
                    return Err(QueryError(i18n::m().q_unterminated.into()));
                }
                let ci = if it.peek() == Some(&'i') {
                    it.next();
                    true
                } else {
                    false
                };
                out.push(Tok::Quoted(s, ci));
            }
            _ => {
                let mut s = String::new();
                while let Some(&ch) = it.peek() {
                    // Operator characters terminate a word, so `app_id='x'` splits
                    // into three tokens
                    if ch.is_whitespace()
                        || ch == '('
                        || ch == ')'
                        || ch == '\''
                        || ch == '"'
                        || ch == '='
                        || ch == '!'
                        || ch == '~'
                    {
                        break;
                    }
                    s.push(ch);
                    it.next();
                }
                out.push(Tok::Word(s));
            }
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn word_is(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }

    fn expr(&mut self) -> Result<Node, QueryError> {
        let mut left = self.and_expr()?;
        while self.word_is("or") {
            self.next();
            let right = self.and_expr()?;
            left = Node::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Node, QueryError> {
        let mut left = self.unary()?;
        while self.word_is("and") {
            self.next();
            let right = self.unary()?;
            left = Node::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Node, QueryError> {
        if self.word_is("not") {
            self.next();
            return Ok(Node::Not(Box::new(self.unary()?)));
        }
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.next();
            let e = self.expr()?;
            if !matches!(self.next(), Some(Tok::RParen)) {
                return Err(QueryError(i18n::m().q_missing_rparen.into()));
            }
            return Ok(e);
        }
        self.test()
    }

    fn test(&mut self) -> Result<Node, QueryError> {
        let field_tok = self
            .next()
            .ok_or_else(|| QueryError(i18n::m().q_unexpected_end.into()))?;
        let field = match field_tok {
            Tok::Word(w) => w,
            Tok::Quoted(..) => return Err(QueryError(i18n::m().q_quoted_field.into())),
            Tok::LParen | Tok::RParen => {
                return Err(QueryError(i18n::m().q_unexpected_paren.into()));
            }
        };

        // String comparison: FIELD OP VALUE
        let op = match self.peek() {
            Some(Tok::Word(w)) if w == "=" => Some(Op::Eq),
            Some(Tok::Word(w)) if w == "!=" => Some(Op::Ne),
            Some(Tok::Word(w)) if w == "~=" => Some(Op::Re),
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let (value, ci) = match self.next() {
                Some(Tok::Quoted(v, ci)) => (v, ci),
                Some(Tok::Word(v)) => (v, false), // tolerate unquoted values
                _ => return Err(QueryError(i18n::fill1(i18n::m().q_missing_value, field))),
            };
            let sf = match field.as_str() {
                "id" => StrField::Id,
                "app_id" => StrField::AppId,
                "title" => StrField::Title,
                "workspace.name" => StrField::WorkspaceName,
                "output.name" => StrField::OutputName,
                other => {
                    return Err(QueryError(i18n::fill1(
                        i18n::m().q_unknown_str_field,
                        other,
                    )));
                }
            };
            return Ok(Node::Str {
                neg: false,
                field: sf,
                op,
                value,
                ci,
            });
        }

        let bf = match field.as_str() {
            "is_active" => BoolField::IsActive,
            "is_active_app_id" => BoolField::IsActiveAppId,
            "is_maximized" => BoolField::IsMaximized,
            "is_minimized" => BoolField::IsMinimized,
            "is_fullscreen" => BoolField::IsFullscreen,
            "is_sticky" => BoolField::IsSticky,
            "workspace.is_visible" => BoolField::WorkspaceIsVisible,
            "workspace.has_focus" => BoolField::WorkspaceHasFocus,
            "output.has_focus" => BoolField::OutputHasFocus,
            other => {
                return Err(QueryError(i18n::fill1(
                    i18n::m().q_unknown_bool_field,
                    other,
                )));
            }
        };
        Ok(Node::Bool {
            neg: false,
            field: bf,
        })
    }
}

#[derive(Debug)]
pub struct Query {
    root: Option<Node>,
}

impl Query {
    /// Parse a query; an empty string matches every window.
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let toks = tokenize(input)?;
        if toks.is_empty() {
            return Ok(Query { root: None });
        }
        let mut p = Parser { toks, pos: 0 };
        let root = p.expr()?;
        if p.pos != p.toks.len() {
            return Err(QueryError(i18n::fill1(i18n::m().q_trailing, p.pos)));
        }
        Ok(Query { root: Some(root) })
    }

    pub fn matches(&self, st: &State, w: &Window) -> bool {
        match &self.root {
            None => true,
            Some(n) => eval(n, st, w),
        }
    }

    /// Whether the query depends on the fields COSMIC pushes in its deferred
    /// batch (state / workspace / output / geometry). Such a query can fail to
    /// match on the `--no-wait` or control-command fast paths because the data
    /// has not arrived yet; callers use this to decide whether to wait once more.
    pub fn uses_deferred_state(&self) -> bool {
        fn walk(n: &Node) -> bool {
            match n {
                Node::Bool { .. } => true,
                Node::Str { field, .. } => {
                    matches!(field, StrField::WorkspaceName | StrField::OutputName)
                }
                Node::Not(a) => walk(a),
                Node::And(a, b) | Node::Or(a, b) => walk(a) || walk(b),
            }
        }
        self.root.as_ref().map(walk).unwrap_or(false)
    }
}

fn eval(node: &Node, st: &State, w: &Window) -> bool {
    match node {
        Node::Not(inner) => !eval(inner, st, w),
        Node::And(a, b) => eval(a, st, w) && eval(b, st, w),
        Node::Or(a, b) => eval(a, st, w) || eval(b, st, w),
        Node::Bool { neg, field } => {
            let v = match field {
                BoolField::IsActive => w.has(ST_ACTIVATED),
                BoolField::IsActiveAppId => st
                    .active_window()
                    .map(|a| a.app_id.is_some() && a.app_id == w.app_id)
                    .unwrap_or(false),
                BoolField::IsMaximized => w.has(ST_MAXIMIZED),
                BoolField::IsMinimized => w.has(ST_MINIMIZED),
                BoolField::IsFullscreen => w.has(ST_FULLSCREEN),
                BoolField::IsSticky => w.has(ST_STICKY),
                BoolField::WorkspaceIsVisible => w
                    .workspace
                    .as_ref()
                    .map(|id| st.workspace_is_visible(id))
                    .unwrap_or(false),
                BoolField::WorkspaceHasFocus => {
                    w.workspace.is_some() && w.workspace == st.active_workspace()
                }
                BoolField::OutputHasFocus => {
                    let o = st.window_output_id(w);
                    o.is_some() && o == st.active_output()
                }
            };
            if *neg { !v } else { v }
        }
        Node::Str {
            neg,
            field,
            op,
            value,
            ci,
        } => {
            let target = match field {
                StrField::Id => w.identifier.clone().unwrap_or_default(),
                StrField::AppId => w.app_id.clone().unwrap_or_default(),
                StrField::Title => w.title.clone().unwrap_or_default(),
                StrField::WorkspaceName => w
                    .workspace
                    .as_ref()
                    .and_then(|id| st.workspace_name(id))
                    .unwrap_or_default()
                    .to_string(),
                StrField::OutputName => st.window_output(w).unwrap_or_default().to_string(),
            };
            let res = match op {
                Op::Eq => {
                    if *ci {
                        target.to_lowercase() == value.to_lowercase()
                    } else {
                        target == *value
                    }
                }
                Op::Ne => {
                    if *ci {
                        target.to_lowercase() != value.to_lowercase()
                    } else {
                        target != *value
                    }
                }
                Op::Re => RegexBuilder::new(value)
                    .case_insensitive(*ci)
                    .build()
                    .map(|re| re.is_match(&target))
                    .unwrap_or(false),
            };
            if *neg { !res } else { res }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ST_ACTIVATED, ST_MAXIMIZED, Window};

    fn win(app: &str, title: &str) -> Window {
        Window {
            app_id: Some(app.to_string()),
            title: Some(title.to_string()),
            ..Default::default()
        }
    }

    fn hit(q: &str, w: &Window) -> bool {
        let st = State::default();
        Query::parse(q).expect("query should parse").matches(&st, w)
    }

    #[test]
    fn empty_query_matches_all() {
        let w = win("firefox", "x");
        assert!(hit("", &w));
        assert!(hit("   ", &w));
    }

    #[test]
    fn equality_and_inequality() {
        let w = win("firefox", "Hello");
        assert!(hit("app_id = 'firefox'", &w));
        assert!(!hit("app_id = 'chrome'", &w));
        assert!(hit("app_id != 'chrome'", &w));
        assert!(!hit("app_id != 'firefox'", &w));
    }

    #[test]
    fn case_sensitivity() {
        let w = win("Firefox", "Hello");
        assert!(!hit("app_id = 'firefox'", &w), "case sensitive by default");
        assert!(hit("app_id = 'firefox'i", &w), "the i suffix ignores case");
        assert!(!hit("app_id != 'firefox'i", &w));
    }

    #[test]
    fn regex_operator() {
        let w = win("com.microsoft.VSCode", "logger.rs - fw-cli-srv");
        assert!(hit("title ~= 'fw-cli'", &w));
        assert!(hit("title ~= '^logger'", &w));
        assert!(!hit("title ~= '^fw-cli'", &w));
        assert!(hit("app_id ~= 'vscode'i", &w));
        assert!(
            !hit("title ~= '('", &w),
            "an invalid regex must not panic, only fail to match"
        );
    }

    #[test]
    fn quoted_values_with_spaces() {
        let w = win("a", "nvim — COSMIC Files");
        assert!(hit("title = 'nvim — COSMIC Files'", &w));
        assert!(hit(r#"title = "nvim — COSMIC Files""#, &w));
    }

    #[test]
    fn escaped_quote_inside_value() {
        let w = win("a", "it's here");
        assert!(hit(r"title = 'it\'s here'", &w));
    }

    #[test]
    fn boolean_fields() {
        let w = Window {
            states: vec![ST_MAXIMIZED, ST_ACTIVATED],
            ..Default::default()
        };
        assert!(hit("is_maximized", &w));
        assert!(hit("is_active", &w));
        assert!(!hit("is_minimized", &w));
        assert!(!hit("is_fullscreen", &w));
        assert!(!hit("is_sticky", &w));
    }

    #[test]
    fn logical_operators_and_precedence() {
        let w = win("firefox", "docs");
        assert!(hit("app_id = 'firefox' and title = 'docs'", &w));
        assert!(!hit("app_id = 'firefox' and title = 'other'", &w));
        assert!(hit("app_id = 'chrome' or title = 'docs'", &w));
        // `and` binds tighter than `or`
        assert!(hit(
            "app_id = 'chrome' and title = 'other' or title = 'docs'",
            &w
        ));
        assert!(!hit(
            "app_id = 'chrome' and (title = 'other' or title = 'docs')",
            &w
        ));
    }

    #[test]
    fn negation() {
        let w = win("firefox", "docs");
        assert!(hit("not app_id = 'chrome'", &w));
        assert!(!hit("not app_id = 'firefox'", &w));
        assert!(hit("not (app_id = 'chrome' or title = 'other')", &w));
        // Window without state: the negation is true.
        assert!(hit("not is_maximized", &w));
        // Window with state: the negation must be false.
        let active = Window {
            states: vec![ST_ACTIVATED],
            ..Default::default()
        };
        assert!(!hit("not is_active", &active));
        assert!(hit("not is_maximized", &active));
    }

    #[test]
    fn parse_errors() {
        assert!(Query::parse("title = 'oops").is_err(), "unterminated quote");
        assert!(Query::parse("bogus = 'x'").is_err(), "unknown string field");
        assert!(
            Query::parse("bogus_field").is_err(),
            "unknown boolean field"
        );
        assert!(
            Query::parse("app_id = ").is_err(),
            "missing comparison value"
        );
        assert!(
            Query::parse("(app_id = 'x'").is_err(),
            "unbalanced parenthesis"
        );
        assert!(
            Query::parse("app_id = 'x' extra").is_err(),
            "trailing content"
        );
        assert!(Query::parse("app_id 'x'").is_err(), "missing operator");
        assert!(Query::parse(")").is_err(), "stray closing parenthesis");
    }

    #[test]
    fn deferred_state_detection() {
        // Queries using only immediate fields (id/app_id/title) need not wait
        // for COSMIC's 150ms batch.
        assert!(!Query::parse("app_id = 'x'").unwrap().uses_deferred_state());
        assert!(
            !Query::parse("title ~= 'x' and id = 'y'")
                .unwrap()
                .uses_deferred_state()
        );
        assert!(!Query::parse("").unwrap().uses_deferred_state());
        assert!(Query::parse("not is_active").unwrap().uses_deferred_state());
        assert!(
            Query::parse("output.name = 'HDMI-A-1'")
                .unwrap()
                .uses_deferred_state()
        );
        assert!(
            Query::parse("workspace.is_visible")
                .unwrap()
                .uses_deferred_state()
        );
    }

    #[test]
    fn operators_without_surrounding_whitespace() {
        // Existing cosmic-ext-window-helper configs write `app_id='x'`, so keep
        // supporting that form
        let w = win("google-chrome", "Chrome");
        assert!(hit("app_id='google-chrome'", &w));
        assert!(!hit("app_id!='google-chrome'", &w));
        assert!(hit("title~='Chrome'", &w));
        assert!(hit("app_id='google-chrome'and title='Chrome'", &w));
        assert!(hit("(app_id='google-chrome')", &w));
        assert!(
            hit("app_id = 'google-chrome'", &w),
            "spaced form still works"
        );
        assert!(hit("title ~= 'Chrome'i", &w));
        // combinator: no spaces + logical operators
        assert!(hit("app_id='a'or app_id='google-chrome'", &w));
    }
}
