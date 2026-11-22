use tower_lsp::lsp_types::{Position, Range};

/// Unified intermediate representation (IR) for a single parsed Zsh document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentIndex {
    pub functions: Vec<FunctionDef>,
    pub variables: Vec<VariableDef>,
    pub aliases: Vec<AliasDef>,
    pub sources: Vec<SourceDirective>,
}

/// Definition of a shell function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDef {
    pub name: String,
    pub name_range: Range,
    pub full_range: Range,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub parent_fn: Option<usize>,
}

/// Syntactic form of a variable declaration or assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableDeclKind {
    /// Direct assignment (`VAR=val`, `VAR+=val`, `VAR[k]=val`).
    Direct,
    /// Keyword declaration (`local`, `export`, `typeset`, `declare`, `readonly`, `integer`, `float`).
    Keyword(&'static str),
    /// `for VAR in ...` or `for VAR (...)` loop variable.
    ForLoop,
    /// `select VAR in ...` loop variable.
    SelectLoop,
    /// C-style `for (( VAR = 0; ... ))` loop variable.
    CForLoop,
    /// `read VAR` / `while read -r VAR` input variable.
    Read,
}

impl VariableDeclKind {
    /// Returns the detail label suitable for `DocumentSymbol::detail`.
    pub fn symbol_detail(&self) -> Option<String> {
        match self {
            VariableDeclKind::Direct => None,
            VariableDeclKind::Keyword(kw) => Some((*kw).to_string()),
            VariableDeclKind::ForLoop => Some("for".to_string()),
            VariableDeclKind::SelectLoop => Some("select".to_string()),
            VariableDeclKind::CForLoop => Some("for (())".to_string()),
            VariableDeclKind::Read => Some("read".to_string()),
        }
    }

    /// Returns `true` if this declaration kind is function-local by default (unless `-g` is passed).
    pub fn is_local_by_default(&self) -> bool {
        matches!(
            self,
            VariableDeclKind::Keyword(
                "local" | "typeset" | "declare" | "integer" | "float" | "readonly"
            )
        )
    }
}

/// Definition or assignment of a shell variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableDef {
    pub name: String,
    pub name_range: Range,
    pub full_range: Range,
    pub kind: VariableDeclKind,
    pub is_function_local: bool,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub enclosing_fn: Option<usize>,
}

/// Definition of a shell alias (`alias name=expansion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasDef {
    pub name: String,
    pub name_range: Range,
    pub full_range: Range,
    pub expansion: Option<String>,
    pub signature: String,
    pub doc_comment: Option<String>,
    pub enclosing_fn: Option<usize>,
}

/// A `source` or `.` script inclusion directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDirective {
    pub path: String,
    pub stmt_range: Range,
    pub arg_end: Position,
}
