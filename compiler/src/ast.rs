#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Program {
    pub functions: Vec<Function>,
    pub externs: Vec<ExternFunction>,
    pub imports: Vec<Import>,
}

impl Program {
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Import {
    pub path: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternFunction {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub library: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Vec<Stmt>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Infer,
    I64,
    Bool,
    Str,
    Void,
}

impl Type {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "infer" => Some(Self::Infer),
            "i64" => Some(Self::I64),
            "bool" => Some(Self::Bool),
            "str" => Some(Self::Str),
            "void" => Some(Self::Void),
            _ => None,
        }
    }

    pub fn is_copy(&self) -> bool {
        matches!(self, Self::Infer | Self::I64 | Self::Bool)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Stmt {
    Let {
        name: String,
        ty: Type,
        expr: Expr,
        line: usize,
    },
    Assign {
        name: String,
        expr: Expr,
        line: usize,
    },
    Return {
        expr: Expr,
        line: usize,
    },
    Expr {
        expr: Expr,
        line: usize,
    },
    IfIs {
        value: Expr,
        arms: Vec<IfIsArm>,
        else_body: Vec<Stmt>,
        line: usize,
    },
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        elif_arms: Vec<ElifArm>,
        else_body: Vec<Stmt>,
        line: usize,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
        line: usize,
    },
    ThreadWhile {
        condition: Expr,
        body: Vec<Stmt>,
        count: Expr,
        wait: bool,
        line: usize,
    },
    ForRange {
        var: String,
        start: Expr,
        end: Expr,
        step: Option<Expr>,
        body: Vec<Stmt>,
        line: usize,
    },
    ThreadCall {
        call: Expr,
        count: Expr,
        wait: bool,
        line: usize,
    },
    Comptime {
        body: Vec<Stmt>,
        line: usize,
    },
    Pass {
        line: usize,
    },
    Break {
        line: usize,
    },
    Continue {
        line: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IfIsArm {
    pub patterns: Vec<IsPattern>,
    pub body: Vec<Stmt>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IsPattern {
    Value(Expr),
    Ne(Expr),
    Lt(Expr),
    Le(Expr),
    Gt(Expr),
    Ge(Expr),
    StartsWith(Expr),
    EndsWith(Expr),
    Contains(Expr),
    Range { start: Expr, end: Expr },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElifArm {
    pub condition: Expr,
    pub body: Vec<Stmt>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Int(i64),
    Bool(bool),
    Str(String),
    Var(String),
    Move(String),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Call {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Not,
}
