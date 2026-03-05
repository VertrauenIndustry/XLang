use crate::ast::{
    BinOp, ElifArm, Expr, ExternFunction, Function, IfIsArm, Import, IsPattern, Param, Program,
    Stmt, Type, UnaryOp,
};
use crate::diag::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Int(i64),
    Str(String),
    Ident(String),
    True,
    False,
    Move,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Not,
    Dot,
    LParen,
    RParen,
    Comma,
}

pub fn parse_program(source: &str) -> Result<Program, Vec<Diagnostic>> {
    let preprocessed = match strip_tilde_brace_comments(source) {
        Ok(s) => s,
        Err(e) => return Err(vec![e]),
    };

    let lines: Vec<(usize, &str)> = preprocessed
        .lines()
        .enumerate()
        .map(|(idx, line)| (idx + 1, line))
        .collect();

    let mut ctx = ParseCtx {
        lines,
        idx: 0,
        errors: Vec::new(),
    };
    let mut functions = Vec::new();
    let mut externs = Vec::new();
    let mut imports = Vec::new();

    while let Some((line_no, line)) = ctx.current() {
        if is_ignorable(line) {
            ctx.idx += 1;
            continue;
        }
        if indent_of(line) != 0 {
            ctx.errors.push(Diagnostic::new(
                line_no,
                "top-level declarations must start at indentation 0",
            ));
            ctx.idx += 1;
            continue;
        }

        let cleaned = strip_inline_comment(line);
        let trimmed = cleaned.trim();
        if trimmed.starts_with("import ") {
            match parse_import_header(line_no, trimmed) {
                Ok(import) => imports.push(import),
                Err(e) => ctx.errors.push(e),
            }
            ctx.idx += 1;
            continue;
        }
        if trimmed.starts_with("extern ") {
            match parse_extern_header(line_no, trimmed) {
                Ok(ext) => externs.push(ext),
                Err(e) => ctx.errors.push(e),
            }
            ctx.idx += 1;
            continue;
        }

        let mut f = match parse_fn_header(line_no, trimmed, true) {
            Ok(f) => f,
            Err(e) => {
                ctx.errors.push(e);
                ctx.idx += 1;
                continue;
            }
        };
        ctx.idx += 1;
        let body = ctx.parse_block(4);
        if body.is_empty() {
            ctx.errors.push(Diagnostic::new(
                line_no,
                format!("function '{}' has an empty body", f.name),
            ));
        } else {
            f.body = body;
            functions.push(f);
        }
    }

    if ctx.errors.is_empty() {
        Ok(Program {
            functions,
            externs,
            imports,
        })
    } else {
        Err(ctx.errors)
    }
}

fn strip_tilde_brace_comments(source: &str) -> Result<String, Diagnostic> {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());

    let mut i = 0usize;
    let mut line_no = 1usize;
    let mut in_string = false;
    let mut escape = false;
    let mut in_tilde_comment = false;
    let mut comment_start_line = 1usize;

    while i < bytes.len() {
        let b = bytes[i];

        if in_tilde_comment {
            if b == b'}' {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'~' {
                    for _ in i..=j {
                        out.push(' ');
                    }
                    i = j + 1;
                    in_tilde_comment = false;
                    continue;
                }
            }

            if b == b'\n' {
                out.push('\n');
                line_no += 1;
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }

        if in_string {
            out.push(b as char);
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            if b == b'\n' {
                line_no += 1;
            }
            i += 1;
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }

        if b == b'~' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                comment_start_line = line_no;
                for _ in i..=j {
                    out.push(' ');
                }
                i = j + 1;
                in_tilde_comment = true;
                continue;
            }
        }

        out.push(b as char);
        if b == b'\n' {
            line_no += 1;
        }
        i += 1;
    }

    if in_tilde_comment {
        return Err(Diagnostic::new(
            comment_start_line,
            "unterminated ~ { ... } ~ comment",
        ));
    }

    Ok(out)
}

struct ParseCtx<'a> {
    lines: Vec<(usize, &'a str)>,
    idx: usize,
    errors: Vec<Diagnostic>,
}

impl<'a> ParseCtx<'a> {
    fn current(&self) -> Option<(usize, &'a str)> {
        self.lines.get(self.idx).copied()
    }

    fn peek_non_ignorable(&self) -> Option<(usize, &'a str)> {
        let mut i = self.idx;
        while let Some((line_no, line)) = self.lines.get(i).copied() {
            if !is_ignorable(line) {
                return Some((line_no, line));
            }
            i += 1;
        }
        None
    }

    fn parse_block(&mut self, indent: usize) -> Vec<Stmt> {
        let mut out = Vec::new();

        while let Some((line_no, line)) = self.current() {
            if is_ignorable(line) {
                self.idx += 1;
                continue;
            }
            let cleaned = strip_inline_comment(line);
            let actual = indent_of(&cleaned);
            if actual < indent {
                break;
            }
            if actual > indent {
                self.errors.push(Diagnostic::new(
                    line_no,
                    format!("unexpected indentation, expected {indent} spaces"),
                ));
                self.idx += 1;
                continue;
            }
            let trimmed = cleaned.trim();
            if trimmed.starts_with("if ") {
                match self.parse_if(line_no, trimmed, indent) {
                    Ok(stmt) => out.push(stmt),
                    Err(e) => {
                        self.errors.push(e);
                        self.idx += 1;
                    }
                }
                continue;
            }
            if trimmed.starts_with("while ") {
                match self.parse_while(line_no, trimmed, indent) {
                    Ok(stmt) => out.push(stmt),
                    Err(e) => {
                        self.errors.push(e);
                        self.idx += 1;
                    }
                }
                continue;
            }
            if trimmed.starts_with("for ") {
                match self.parse_for_range(line_no, trimmed, indent) {
                    Ok(stmt) => out.push(stmt),
                    Err(e) => {
                        self.errors.push(e);
                        self.idx += 1;
                    }
                }
                continue;
            }
            if trimmed == "comptime:" {
                self.idx += 1;
                let body = self.parse_block(indent + 4);
                if body.is_empty() {
                    self.errors.push(Diagnostic::new(
                        line_no,
                        "comptime block cannot have an empty body",
                    ));
                } else {
                    out.push(Stmt::Comptime {
                        body,
                        line: line_no,
                    });
                }
                continue;
            }

            match parse_simple_stmt(line_no, trimmed) {
                Ok(stmt) => out.push(stmt),
                Err(e) => self.errors.push(e),
            }
            self.idx += 1;
        }

        out
    }

    fn parse_if(
        &mut self,
        line_no: usize,
        trimmed: &str,
        base_indent: usize,
    ) -> Result<Stmt, Diagnostic> {
        let cond_raw = trimmed
            .trim_start_matches("if")
            .trim()
            .trim_end_matches(':')
            .trim()
            .to_string();
        if cond_raw.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "if statement requires a condition/value",
            ));
        }
        let condition = parse_expr(line_no, &cond_raw)?;
        self.idx += 1;

        if self.looks_like_if_is_arm(base_indent + 4) {
            return self.parse_if_is_tail(line_no, condition, base_indent);
        }
        self.parse_standard_if_tail(line_no, condition, base_indent)
    }

    fn looks_like_if_is_arm(&self, indent: usize) -> bool {
        let Some((_, line)) = self.peek_non_ignorable() else {
            return false;
        };
        if indent_of(line) != indent {
            return false;
        }
        let t = line.trim();
        t.starts_with("is ") || t == "else:"
    }

    fn parse_if_is_tail(
        &mut self,
        line_no: usize,
        value: Expr,
        base_indent: usize,
    ) -> Result<Stmt, Diagnostic> {
        let mut arms = Vec::new();
        let mut else_body = Vec::new();

        loop {
            let Some((arm_line, arm_raw)) = self.current() else {
                break;
            };
            if is_ignorable(arm_raw) {
                self.idx += 1;
                continue;
            }
            let arm_cleaned = strip_inline_comment(arm_raw);
            let arm_indent = indent_of(&arm_cleaned);
            if arm_indent < base_indent + 4 {
                break;
            }
            if arm_indent > base_indent + 4 {
                self.errors.push(Diagnostic::new(
                    arm_line,
                    format!("arm indentation must be exactly {} spaces", base_indent + 4),
                ));
                self.idx += 1;
                continue;
            }

            let arm_trim = arm_cleaned.trim();
            if arm_trim == "else:" {
                if !else_body.is_empty() {
                    return Err(Diagnostic::new(
                        arm_line,
                        "duplicate default branch in if-is statement",
                    ));
                }
                self.idx += 1;
                else_body = self.parse_block(base_indent + 8);
                continue;
            }
            if !arm_trim.starts_with("is ") || !arm_trim.ends_with(':') {
                return Err(Diagnostic::new(
                    arm_line,
                    "inside `if` blocks use `is <pattern>:` arms or `else:`",
                ));
            }
            let pattern_raw = arm_trim
                .trim_start_matches("is")
                .trim()
                .trim_end_matches(':')
                .trim();

            if matches!(
                pattern_raw,
                "default" | "none" | "null" | "Default" | "None" | "Null"
            ) {
                if !else_body.is_empty() {
                    return Err(Diagnostic::new(
                        arm_line,
                        "duplicate default branch in if-is statement",
                    ));
                }
                self.idx += 1;
                else_body = self.parse_block(base_indent + 8);
                continue;
            }

            let pattern_parts = split_pattern_list(arm_line, pattern_raw)?;
            if pattern_parts.is_empty() {
                return Err(Diagnostic::new(
                    arm_line,
                    "`is` arm must contain at least one pattern",
                ));
            }
            let mut patterns = Vec::new();
            for p in pattern_parts {
                patterns.push(parse_is_pattern(arm_line, &p)?);
            }

            self.idx += 1;
            let body = self.parse_block(base_indent + 8);
            if body.is_empty() {
                return Err(Diagnostic::new(
                    arm_line,
                    "`is` arm cannot have an empty body",
                ));
            }
            arms.push(IfIsArm {
                patterns,
                body,
                line: arm_line,
            });
        }

        if arms.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "`if` statement requires at least one `is <pattern>:` arm",
            ));
        }

        Ok(Stmt::IfIs {
            value,
            arms,
            else_body,
            line: line_no,
        })
    }

    fn parse_standard_if_tail(
        &mut self,
        line_no: usize,
        condition: Expr,
        base_indent: usize,
    ) -> Result<Stmt, Diagnostic> {
        let then_body = self.parse_block(base_indent + 4);
        if then_body.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "if statement cannot have an empty body",
            ));
        }

        let mut elif_arms = Vec::new();
        let mut else_body = Vec::new();

        loop {
            let Some((next_line, raw)) = self.current() else {
                break;
            };
            if is_ignorable(raw) {
                self.idx += 1;
                continue;
            }
            let cleaned = strip_inline_comment(raw);
            if indent_of(&cleaned) != base_indent {
                break;
            }
            let t = cleaned.trim();
            if t.starts_with("elif ") {
                let cond_raw = t
                    .trim_start_matches("elif")
                    .trim()
                    .trim_end_matches(':')
                    .trim();
                if cond_raw.is_empty() {
                    return Err(Diagnostic::new(
                        next_line,
                        "elif statement requires a condition",
                    ));
                }
                let cond = parse_expr(next_line, cond_raw)?;
                self.idx += 1;
                let body = self.parse_block(base_indent + 4);
                if body.is_empty() {
                    return Err(Diagnostic::new(
                        next_line,
                        "elif statement cannot have an empty body",
                    ));
                }
                elif_arms.push(ElifArm {
                    condition: cond,
                    body,
                    line: next_line,
                });
                continue;
            }
            if t == "else:" {
                self.idx += 1;
                else_body = self.parse_block(base_indent + 4);
            }
            break;
        }

        Ok(Stmt::If {
            condition,
            then_body,
            elif_arms,
            else_body,
            line: line_no,
        })
    }

    fn parse_while(
        &mut self,
        line_no: usize,
        trimmed: &str,
        base_indent: usize,
    ) -> Result<Stmt, Diagnostic> {
        let (while_head, thread_suffix) = split_thread_suffix(trimmed);
        let cond_raw = while_head
            .trim_start_matches("while")
            .trim()
            .trim_end_matches(':')
            .trim();
        if cond_raw.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "while statement requires a condition",
            ));
        }
        let condition = parse_expr(line_no, cond_raw)?;
        self.idx += 1;
        let body = self.parse_block(base_indent + 4);
        if body.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "while statement cannot have an empty body",
            ));
        }
        if let Some(spec) = thread_suffix {
            let (count, wait) = parse_thread_spec(line_no, spec)?;
            Ok(Stmt::ThreadWhile {
                condition,
                body,
                count,
                wait,
                line: line_no,
            })
        } else {
            Ok(Stmt::While {
                condition,
                body,
                line: line_no,
            })
        }
    }

    fn parse_for_range(
        &mut self,
        line_no: usize,
        trimmed: &str,
        base_indent: usize,
    ) -> Result<Stmt, Diagnostic> {
        let header = trimmed
            .trim_start_matches("for")
            .trim()
            .trim_end_matches(':')
            .trim();
        let (var_raw, rest_raw) = header.split_once(" in ").ok_or_else(|| {
            Diagnostic::new(
                line_no,
                "for statement must use `for <name> in <start>..<end>:`",
            )
        })?;

        let var = var_raw.trim();
        if !is_ident(var) {
            return Err(Diagnostic::new(
                line_no,
                "for loop variable must be a valid identifier",
            ));
        }
        let (range_raw, step_raw) =
            if let Some((range_raw, step_raw)) = rest_raw.split_once(" step ") {
                (range_raw.trim(), Some(step_raw.trim()))
            } else {
                (rest_raw.trim(), None)
            };

        let (start_raw, end_raw) = range_raw.split_once("..").ok_or_else(|| {
            Diagnostic::new(
                line_no,
                "for range must use `..` syntax: for i in start..end:",
            )
        })?;

        let start_raw = start_raw.trim();
        let end_raw = end_raw.trim();
        if start_raw.is_empty() || end_raw.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "for range bounds must both be present",
            ));
        }
        let start = parse_expr(line_no, start_raw)?;
        let end = parse_expr(line_no, end_raw)?;
        let step = if let Some(raw) = step_raw {
            if raw.is_empty() {
                return Err(Diagnostic::new(line_no, "for step expression is missing"));
            }
            Some(parse_expr(line_no, raw)?)
        } else {
            None
        };

        self.idx += 1;
        let body = self.parse_block(base_indent + 4);
        if body.is_empty() {
            return Err(Diagnostic::new(
                line_no,
                "for statement cannot have an empty body",
            ));
        }
        Ok(Stmt::ForRange {
            var: var.to_string(),
            start,
            end,
            step,
            body,
            line: line_no,
        })
    }
}

fn is_ignorable(line: &str) -> bool {
    let stripped = strip_inline_comment(line);
    let trimmed = stripped.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn indent_of(line: &str) -> usize {
    let stripped = strip_inline_comment(line);
    stripped.chars().take_while(|c| *c == ' ').count()
}

fn parse_import_header(line: usize, header: &str) -> Result<Import, Diagnostic> {
    let raw = header
        .strip_prefix("import ")
        .ok_or_else(|| Diagnostic::new(line, "invalid import syntax"))?
        .trim();
    if !(raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2) {
        return Err(Diagnostic::new(
            line,
            "import path must be a quoted string: import \"module.x\"",
        ));
    }
    Ok(Import {
        path: raw[1..raw.len() - 1].to_string(),
        line,
    })
}

fn parse_extern_header(line: usize, header: &str) -> Result<ExternFunction, Diagnostic> {
    let rest = header
        .strip_prefix("extern ")
        .ok_or_else(|| Diagnostic::new(line, "invalid extern syntax"))?;
    let (func, lib_raw) = rest
        .rsplit_once(" from ")
        .ok_or_else(|| Diagnostic::new(line, "extern must include `from \"library\"`"))?;
    let lib = lib_raw.trim();
    if !(lib.starts_with('"') && lib.ends_with('"') && lib.len() >= 2) {
        return Err(Diagnostic::new(
            line,
            "extern library path must be a quoted string",
        ));
    }
    let library = lib[1..lib.len() - 1].to_string();
    let function = parse_fn_header(line, func, false)?;
    Ok(ExternFunction {
        name: function.name,
        params: function.params,
        ret: function.ret,
        library,
        line,
    })
}

fn parse_fn_header(
    line: usize,
    header: &str,
    require_body_colon: bool,
) -> Result<Function, Diagnostic> {
    let keyword = if header.starts_with("fn ") {
        "fn"
    } else if header.starts_with("def ") {
        "def"
    } else if header.starts_with("function ") {
        "function"
    } else {
        ""
    };
    if keyword.is_empty() {
        return Err(Diagnostic::new(
            line,
            "expected function declaration: fn|def|function name(args) [-> type]:",
        ));
    }

    if require_body_colon && !header.ends_with(':') {
        return Err(Diagnostic::new(
            line,
            "function declaration must end with ':'",
        ));
    }

    let without_colon = if let Some(stripped) = header.strip_suffix(':') {
        stripped
    } else {
        header
    };

    let rest = without_colon[keyword.len()..].trim_start();
    let (name, after_name) = rest
        .split_once('(')
        .ok_or_else(|| Diagnostic::new(line, "expected '(' after function name"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Diagnostic::new(line, "function name is missing"));
    }

    let (params_raw, after_params) = after_name
        .split_once(')')
        .ok_or_else(|| Diagnostic::new(line, "expected ')' after parameters"))?;

    let mut params = Vec::new();
    for raw in params_raw.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let (pname, pty) = if let Some((pname, pty_raw)) = raw.split_once(':') {
            let pty = Type::parse(pty_raw).ok_or_else(|| {
                Diagnostic::new(line, format!("unknown parameter type '{pty_raw}'"))
            })?;
            (pname.trim().to_string(), pty)
        } else {
            (raw.to_string(), Type::Infer)
        };
        params.push(Param {
            name: pname,
            ty: pty,
        });
    }

    let after_params = after_params.trim();
    let ret = if after_params.is_empty() {
        Type::Infer
    } else if after_params.starts_with("->") {
        let ret_raw = after_params.trim_start_matches("->").trim();
        Type::parse(ret_raw)
            .ok_or_else(|| Diagnostic::new(line, format!("unknown return type '{ret_raw}'")))?
    } else {
        return Err(Diagnostic::new(
            line,
            "invalid function header suffix; expected optional '-> type'",
        ));
    };

    Ok(Function {
        name: name.to_string(),
        params,
        ret,
        body: Vec::new(),
        line,
    })
}

fn parse_simple_stmt(line: usize, stmt: &str) -> Result<Stmt, Diagnostic> {
    if stmt == "pass" {
        return Ok(Stmt::Pass { line });
    }
    if stmt == "break" {
        return Ok(Stmt::Break { line });
    }
    if stmt == "continue" {
        return Ok(Stmt::Continue { line });
    }

    if let Some(rest) = stmt.strip_prefix("let ") {
        let (name_ty, expr_raw) = rest
            .split_once('=')
            .ok_or_else(|| Diagnostic::new(line, "let statement must have '='"))?;
        let (name, ty) = if let Some((name, ty_raw)) = name_ty.split_once(':') {
            let ty = Type::parse(ty_raw.trim()).ok_or_else(|| {
                Diagnostic::new(line, format!("unknown type '{}'", ty_raw.trim()))
            })?;
            (name.trim().to_string(), ty)
        } else {
            (name_ty.trim().to_string(), Type::Infer)
        };
        let expr = parse_expr(line, expr_raw.trim())?;
        return Ok(Stmt::Let {
            name,
            ty,
            expr,
            line,
        });
    }

    if let Some(rest) = stmt.strip_prefix("return ") {
        let expr = parse_expr(line, rest.trim())?;
        return Ok(Stmt::Return { expr, line });
    }

    if let Some((lhs, rhs, op)) = split_compound_assignment(stmt) {
        let lhs = lhs.trim();
        if is_ident(lhs) {
            let rhs_expr = parse_expr(line, rhs.trim())?;
            let expr = Expr::Binary {
                op,
                left: Box::new(Expr::Var(lhs.to_string())),
                right: Box::new(rhs_expr),
            };
            return Ok(Stmt::Assign {
                name: lhs.to_string(),
                expr,
                line,
            });
        }
    }

    if let Some((lhs, rhs)) = split_assignment(stmt) {
        let lhs = lhs.trim();
        if is_ident(lhs) {
            let expr = parse_expr(line, rhs.trim())?;
            return Ok(Stmt::Assign {
                name: lhs.to_string(),
                expr,
                line,
            });
        }
    }

    let (expr_raw, thread_suffix) = split_thread_suffix(stmt);
    let expr = parse_expr(line, expr_raw.trim())?;
    if let Some(spec) = thread_suffix {
        if !matches!(expr, Expr::Call { .. }) {
            return Err(Diagnostic::new(
                line,
                "thread() syntax currently supports only function call statements",
            ));
        }
        let (count, wait) = parse_thread_spec(line, spec)?;
        return Ok(Stmt::ThreadCall {
            call: expr,
            count,
            wait,
            line,
        });
    }
    Ok(Stmt::Expr { expr, line })
}

fn parse_expr(line: usize, raw: &str) -> Result<Expr, Diagnostic> {
    let tokens = tokenize(line, raw)?;
    let mut p = ExprParser::new(tokens, line);
    let expr = p.parse_expr(0)?;
    if p.peek().is_some() {
        return Err(Diagnostic::new(
            line,
            "unexpected trailing tokens in expression",
        ));
    }
    Ok(expr)
}

fn tokenize(line: usize, raw: &str) -> Result<Vec<Tok>, Diagnostic> {
    let mut tokens = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            b'+' => {
                tokens.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                tokens.push(Tok::Minus);
                i += 1;
            }
            b'*' => {
                tokens.push(Tok::Star);
                i += 1;
            }
            b'/' => {
                tokens.push(Tok::Slash);
                i += 1;
            }
            b'%' => {
                tokens.push(Tok::Percent);
                i += 1;
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Tok::EqEq);
                    i += 2;
                } else {
                    return Err(Diagnostic::new(line, "unexpected '=' in expression"));
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Tok::NotEq);
                    i += 2;
                } else {
                    return Err(Diagnostic::new(line, "unexpected '!' in expression"));
                }
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Tok::Lte);
                    i += 2;
                } else {
                    tokens.push(Tok::Lt);
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    tokens.push(Tok::Gte);
                    i += 2;
                } else {
                    tokens.push(Tok::Gt);
                    i += 1;
                }
            }
            b'(' => {
                tokens.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Tok::RParen);
                i += 1;
            }
            b'.' => {
                tokens.push(Tok::Dot);
                i += 1;
            }
            b',' => {
                tokens.push(Tok::Comma);
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let Some(end_rel) = raw[start..].find('"') else {
                    return Err(Diagnostic::new(line, "unterminated string literal"));
                };
                let end = start + end_rel;
                let s = raw[start..end].to_string();
                tokens.push(Tok::Str(s));
                i = end + 1;
            }
            ch if ch.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let parsed = raw[start..i]
                    .parse::<i64>()
                    .map_err(|_| Diagnostic::new(line, "invalid integer literal"))?;
                tokens.push(Tok::Int(parsed));
            }
            ch if is_ident_start_byte(ch) => {
                let start = i;
                while i < bytes.len() && is_ident_continue_byte(bytes[i]) {
                    i += 1;
                }
                let ident = raw[start..i].to_string();
                match ident.as_str() {
                    "true" => tokens.push(Tok::True),
                    "false" => tokens.push(Tok::False),
                    "move" => tokens.push(Tok::Move),
                    "and" => tokens.push(Tok::And),
                    "or" => tokens.push(Tok::Or),
                    "not" => tokens.push(Tok::Not),
                    _ => tokens.push(Tok::Ident(ident)),
                }
            }
            _ => {
                return Err(Diagnostic::new(
                    line,
                    format!("unexpected character '{}'", bytes[i] as char),
                ));
            }
        }
    }
    Ok(tokens)
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if is_ident_start(ch) => chars.all(is_ident_continue),
        _ => false,
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn is_ident_start_byte(ch: u8) -> bool {
    ch == b'_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue_byte(ch: u8) -> bool {
    ch == b'_' || ch.is_ascii_alphanumeric()
}

struct ExprParser {
    tokens: Vec<Tok>,
    idx: usize,
    line: usize,
}

impl ExprParser {
    fn new(tokens: Vec<Tok>, line: usize) -> Self {
        Self {
            tokens,
            idx: 0,
            line,
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.idx)
    }

    fn next(&mut self) -> Option<Tok> {
        let token = self.tokens.get(self.idx).cloned();
        if token.is_some() {
            self.idx += 1;
        }
        token
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_prefix()?;

        loop {
            let op = match self.peek() {
                Some(Tok::Or) => BinOp::Or,
                Some(Tok::And) => BinOp::And,
                Some(Tok::EqEq) => BinOp::Eq,
                Some(Tok::NotEq) => BinOp::Ne,
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Lte) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Gte) => BinOp::Ge,
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            let (l_bp, r_bp) = infix_bp(op);
            if l_bp < min_bp {
                break;
            }
            self.next();
            let rhs = self.parse_expr(r_bp)?;
            lhs = Expr::Binary {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }

        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, Diagnostic> {
        let base = match self.next() {
            Some(Tok::Int(v)) => Ok(Expr::Int(v)),
            Some(Tok::True) => Ok(Expr::Bool(true)),
            Some(Tok::False) => Ok(Expr::Bool(false)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::Minus) => {
                let expr = self.parse_expr(11)?;
                Ok(Expr::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr),
                })
            }
            Some(Tok::Not) => {
                let expr = self.parse_expr(11)?;
                Ok(Expr::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr),
                })
            }
            Some(Tok::Move) => match self.next() {
                Some(Tok::Ident(name)) => Ok(Expr::Move(name)),
                _ => Err(Diagnostic::new(
                    self.line,
                    "expected identifier after 'move'",
                )),
            },
            Some(Tok::Ident(name)) => {
                let mut expr = Expr::Var(name.clone());
                if matches!(self.peek(), Some(Tok::LParen)) {
                    let args = self.parse_call_args("function call")?;
                    expr = Expr::Call { name, args };
                }
                Ok(expr)
            }
            Some(Tok::LParen) => {
                let expr = self.parse_expr(0)?;
                match self.next() {
                    Some(Tok::RParen) => Ok(expr),
                    _ => Err(Diagnostic::new(self.line, "expected ')'")),
                }
            }
            other => Err(Diagnostic::new(
                self.line,
                format!("invalid expression start: {other:?}"),
            )),
        }?;
        self.parse_postfix(base)
    }

    fn parse_call_args(&mut self, context: &str) -> Result<Vec<Expr>, Diagnostic> {
        match self.next() {
            Some(Tok::LParen) => {}
            _ => {
                return Err(Diagnostic::new(
                    self.line,
                    format!("expected '(' after {context}"),
                ));
            }
        }

        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Tok::RParen)) {
            loop {
                args.push(self.parse_expr(0)?);
                match self.peek() {
                    Some(Tok::Comma) => {
                        self.next();
                    }
                    Some(Tok::RParen) => break,
                    _ => {
                        return Err(Diagnostic::new(
                            self.line,
                            "expected ',' or ')' in call arguments",
                        ));
                    }
                }
            }
        }
        match self.next() {
            Some(Tok::RParen) => Ok(args),
            _ => Err(Diagnostic::new(self.line, "expected ')' after call")),
        }
    }

    fn parse_postfix(&mut self, mut expr: Expr) -> Result<Expr, Diagnostic> {
        loop {
            if !matches!(self.peek(), Some(Tok::Dot)) {
                break;
            }
            self.next();
            let method = match self.next() {
                Some(Tok::Ident(name)) => name,
                _ => {
                    return Err(Diagnostic::new(
                        self.line,
                        "expected method name after '.'",
                    ));
                }
            };
            let args = self.parse_call_args("method name")?;
            let mut rewritten = Vec::with_capacity(args.len() + 1);
            rewritten.push(expr);
            rewritten.extend(args);
            expr = Expr::Call {
                name: method,
                args: rewritten,
            };
        }
        Ok(expr)
    }
}

fn infix_bp(op: BinOp) -> (u8, u8) {
    match op {
        BinOp::Or => (1, 2),
        BinOp::And => (3, 4),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (5, 6),
        BinOp::Add | BinOp::Sub => (7, 8),
        BinOp::Mul | BinOp::Div | BinOp::Mod => (9, 10),
    }
}

fn split_assignment(stmt: &str) -> Option<(&str, &str)> {
    let bytes = stmt.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string && b == b'=' {
            let prev = if i > 0 { bytes[i - 1] } else { b'\0' };
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b'\0'
            };
            if prev != b'=' && prev != b'<' && prev != b'>' && prev != b'!' && next != b'=' {
                return Some((&stmt[..i], &stmt[i + 1..]));
            }
        }
        i += 1;
    }
    None
}

fn split_compound_assignment(stmt: &str) -> Option<(&str, &str, BinOp)> {
    const OPS: [(&str, BinOp); 5] = [
        ("+=", BinOp::Add),
        ("-=", BinOp::Sub),
        ("*=", BinOp::Mul),
        ("/=", BinOp::Div),
        ("%=", BinOp::Mod),
    ];
    for (raw, op) in OPS {
        if let Some((lhs, rhs)) = stmt.split_once(raw) {
            return Some((lhs, rhs, op));
        }
    }
    None
}

fn split_thread_suffix(stmt: &str) -> (&str, Option<&str>) {
    let bytes = stmt.as_bytes();
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            let left = stmt[..i].trim_end();
            let right = stmt[i + 2..].trim_start();
            return (left, Some(right));
        }
        i += 1;
    }
    (stmt, None)
}

fn parse_thread_spec(line: usize, raw: &str) -> Result<(Expr, bool), Diagnostic> {
    let trimmed = raw.trim();
    let (head, wait) = if let Some(prefix) = trimmed.strip_suffix(".wait()") {
        (prefix.trim_end(), true)
    } else {
        (trimmed, false)
    };
    if !head.starts_with("thread(") || !head.ends_with(')') {
        return Err(Diagnostic::new(
            line,
            "thread suffix must be thread() / thread(n) / thread(n).wait()",
        ));
    }
    let inside = head["thread(".len()..head.len() - 1].trim();
    let count = if inside.is_empty() {
        Expr::Int(1)
    } else {
        parse_expr(line, inside)?
    };
    Ok((count, wait))
}

fn strip_inline_comment(line: &str) -> String {
    let mut in_string = false;
    let mut escape = false;

    for (idx, ch) in line.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == '#' {
            return line[..idx].to_string();
        }
        if ch == '/' && line[idx + 1..].starts_with('/') {
            return line[..idx].to_string();
        }
    }
    line.to_string()
}

fn split_pattern_list(line: usize, raw: &str) -> Result<Vec<String>, Diagnostic> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escape = false;

    for ch in raw.chars() {
        if in_string {
            current.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err(Diagnostic::new(line, "unbalanced ')' in is-pattern list"));
                }
                current.push(ch);
            }
            ',' if depth == 0 => {
                let part = current.trim();
                if !part.is_empty() {
                    out.push(part.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if in_string {
        return Err(Diagnostic::new(
            line,
            "unterminated string in is-pattern list",
        ));
    }
    if depth != 0 {
        return Err(Diagnostic::new(
            line,
            "unbalanced parentheses in is-pattern list",
        ));
    }

    let part = current.trim();
    if !part.is_empty() {
        out.push(part.to_string());
    }
    Ok(out)
}

fn parse_is_pattern(line: usize, raw: &str) -> Result<IsPattern, Diagnostic> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Diagnostic::new(line, "empty `is` pattern"));
    }

    if let Some(rest) = raw.strip_prefix("starts_with ") {
        return Ok(IsPattern::StartsWith(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix("ends_with ") {
        return Ok(IsPattern::EndsWith(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix("contains ") {
        return Ok(IsPattern::Contains(parse_expr(line, rest.trim())?));
    }

    if let Some(rest) = raw.strip_prefix("!=") {
        return Ok(IsPattern::Ne(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix("<=") {
        return Ok(IsPattern::Le(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix(">=") {
        return Ok(IsPattern::Ge(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix("<") {
        return Ok(IsPattern::Lt(parse_expr(line, rest.trim())?));
    }
    if let Some(rest) = raw.strip_prefix(">") {
        return Ok(IsPattern::Gt(parse_expr(line, rest.trim())?));
    }

    if let Some((start_raw, end_raw)) = raw.split_once("..") {
        let start = parse_expr(line, start_raw.trim())?;
        let end = parse_expr(line, end_raw.trim())?;
        return Ok(IsPattern::Range { start, end });
    }

    Ok(IsPattern::Value(parse_expr(line, raw)?))
}
