//! MilkDrop preset 方程式的直譯器。
//!
//! butterchurn 的 preset 包裡，`*_eqs_str` **已經是 JavaScript 而不是原始的
//! NS-EEL**（它的建置流程先轉過一遍）。所以這裡要處理的是一個很窄的 JS 子集，
//! 但不是純運算式 —— 實際用到的東西掃過全部 526 個 preset、3106 段程式之後是：
//!
//! ```text
//!   賦值 = += -= *= /=   鏈式賦值 b = a.n = 0
//!   三元 ?:              逗號運算子
//!   && ||  比較  算術     後置 ++ --
//!   C 風格 for、while、do{}while、if
//!   a.name、a["name"]（因為 var 是保留字）
//!   a.megabuf[i]、a.gmegabuf[i]
//!   Math.{sin,cos,tan,asin,atan,atan2,abs,floor,min,max,log,exp}
//!   div above below equal bnot mod pow sqrt sqr sign sigmoid bitand band bor randint
//! ```
//!
//! 為什麼不嵌 QuickJS：這些式子跑在很內層的迴圈裡 —— `pixel_eqs` 是每個網格
//! 頂點都要算一次（48×36 ≈ 1700 次／幀），`point_eqs` 是每個波形取樣點。
//! 把變數解析成**槽位索引**之後用樹走訪直譯，比跨 FFI 每次重新查名字快得多。

use std::collections::HashMap;

// ---------------------------------------------------------------- 詞法

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Str(String),
    Punct(&'static str),
    Eof,
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

/// 多字元運算子要先試長的，不然 `<=` 會被切成 `<` 加 `=`。
const PUNCTS: &[&str] = &[
    "===", "!==", "<<", ">>", "==", "!=", "<=", ">=", "&&", "||", "++", "--", "+=", "-=", "*=",
    "/=", "%=", "+", "-", "*", "/", "%", "!", "<", ">", "=", "?", ":", "(", ")", "[", "]", "{",
    "}", ";", ",", ".", "&", "|", "^", "~",
];

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self { s: s.as_bytes(), i: 0 }
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_ascii_whitespace() {
                self.i += 1;
            } else if c == b'/' && self.i + 1 < self.s.len() && self.s[self.i + 1] == b'/' {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn next(&mut self) -> Result<Tok, String> {
        self.skip_ws();
        if self.i >= self.s.len() {
            return Ok(Tok::Eof);
        }
        let c = self.s[self.i];

        // 數字：123 / .5 / 1.5e-3 / 1E4
        if c.is_ascii_digit() || (c == b'.' && self.peek_digit(1)) {
            let start = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.')
            {
                self.i += 1;
            }
            if self.i < self.s.len() && (self.s[self.i] | 32) == b'e' {
                let save = self.i;
                self.i += 1;
                if self.i < self.s.len() && (self.s[self.i] == b'+' || self.s[self.i] == b'-') {
                    self.i += 1;
                }
                if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                } else {
                    self.i = save;
                }
            }
            let txt = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("0");
            return txt
                .parse::<f64>()
                .map(Tok::Num)
                .map_err(|_| format!("數字解不開：{txt}"));
        }

        // 識別字
        if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            let start = self.i;
            while self.i < self.s.len() {
                let d = self.s[self.i];
                if d.is_ascii_alphanumeric() || d == b'_' || d == b'$' {
                    self.i += 1;
                } else {
                    break;
                }
            }
            let txt = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("").to_string();
            return Ok(Tok::Ident(txt));
        }

        // 字串（只出現在 a["var"] 這種保留字的成員存取）
        if c == b'"' || c == b'\'' {
            let quote = c;
            self.i += 1;
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != quote {
                if self.s[self.i] == b'\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            let txt = std::str::from_utf8(&self.s[start..self.i.min(self.s.len())])
                .unwrap_or("")
                .to_string();
            self.i = (self.i + 1).min(self.s.len());
            return Ok(Tok::Str(txt));
        }

        for p in PUNCTS {
            if self.s[self.i..].starts_with(p.as_bytes()) {
                self.i += p.len();
                return Ok(Tok::Punct(p));
            }
        }
        Err(format!("看不懂的字元 {:?}", c as char))
    }

    fn peek_digit(&self, off: usize) -> bool {
        self.s.get(self.i + off).is_some_and(|d| d.is_ascii_digit())
    }
}

// ---------------------------------------------------------------- 語法樹

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fun {
    Sin, Cos, Tan, Asin, Acos, Atan, Atan2, Abs, Floor, Ceil, Min, Max, Log, Log10, Exp,
    Pow, Sqrt, Random,
    Div, Above, Below, Equal, Bnot, Mod, Sqr, Sign, Sigmoid, BitAnd, Band, Bor, RandInt,
    Rand, Int, Invsqrt,
}

#[derive(Debug, Clone)]
pub enum Place {
    Var(usize),
    Mega(Box<Expr>),
    GMega(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Var(usize),
    Mega(Box<Expr>),
    GMega(Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `op` 是 None 代表單純的 `=`
    Assign(Place, Option<BinOp>, Box<Expr>),
    /// 後置 ++／--，回傳舊值
    Post(usize, f64),
    Call(Fun, Vec<Expr>),
    Comma(Vec<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For(Option<Expr>, Option<Expr>, Option<Expr>, Box<Stmt>),
    Empty,
}

// ---------------------------------------------------------------- 變數表

/// 名稱 → 槽位。整個 preset 共用一張，因為 init/frame/pixel 之間要互通。
#[derive(Default, Debug)]
pub struct Vars {
    slots: HashMap<String, usize>,
    names: Vec<String>,
}

impl Vars {
    pub fn slot(&mut self, name: &str) -> usize {
        if let Some(&i) = self.slots.get(name) {
            return i;
        }
        let i = self.names.len();
        self.slots.insert(name.to_string(), i);
        self.names.push(name.to_string());
        i
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.slots.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[allow(dead_code)] // 配合 len()
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    #[allow(dead_code)]
    pub fn name_of(&self, slot: usize) -> &str {
        self.names.get(slot).map(|s| s.as_str()).unwrap_or("?")
    }
}

// ---------------------------------------------------------------- 剖析

pub struct Parser<'v> {
    toks: Vec<Tok>,
    p: usize,
    vars: &'v mut Vars,
}

/// 解析一段程式。`vars` 會累積這段用到的所有變數槽位。
pub fn parse(src: &str, vars: &mut Vars) -> Result<Vec<Stmt>, String> {
    let mut lex = Lexer::new(src);
    let mut toks = Vec::new();
    loop {
        let t = lex.next()?;
        let end = t == Tok::Eof;
        toks.push(t);
        if end {
            break;
        }
    }
    let mut ps = Parser { toks, p: 0, vars };
    let mut out = Vec::new();
    while !ps.at_eof() {
        out.push(ps.stmt()?);
    }
    Ok(out)
}

impl<'v> Parser<'v> {
    fn at_eof(&self) -> bool {
        matches!(self.toks.get(self.p), Some(Tok::Eof) | None)
    }

    fn peek(&self) -> &Tok {
        self.toks.get(self.p).unwrap_or(&Tok::Eof)
    }

    fn eat_punct(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Tok::Punct(p) if *p == s) {
            self.p += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.eat_punct(s) {
            Ok(())
        } else {
            Err(format!("第 {} 個 token 應該是 {s:?}，實際是 {:?}", self.p, self.peek()))
        }
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if matches!(self.peek(), Tok::Ident(i) if i == kw) {
            self.p += 1;
            true
        } else {
            false
        }
    }

    // ---- 語句 ----

    fn stmt(&mut self) -> Result<Stmt, String> {
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if self.eat_punct("{") {
            let mut body = Vec::new();
            while !self.eat_punct("}") {
                if self.at_eof() {
                    return Err("區塊沒有收尾的 }".into());
                }
                body.push(self.stmt()?);
            }
            return Ok(Stmt::Block(body));
        }
        if self.eat_kw("if") {
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            let t = self.stmt()?;
            let e = if self.eat_kw("else") { Some(Box::new(self.stmt()?)) } else { None };
            return Ok(Stmt::If(c, Box::new(t), e));
        }
        if self.eat_kw("while") {
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            let b = self.stmt()?;
            return Ok(Stmt::While(c, Box::new(b)));
        }
        if self.eat_kw("do") {
            let b = self.stmt()?;
            if !self.eat_kw("while") {
                return Err("do 後面少了 while".into());
            }
            self.expect("(")?;
            let c = self.expr()?;
            self.expect(")")?;
            self.eat_punct(";");
            return Ok(Stmt::DoWhile(Box::new(b), c));
        }
        if self.eat_kw("for") {
            self.expect("(")?;
            // `for(var b=…)` 的 var 直接吞掉，這裡沒有作用域的概念
            self.eat_kw("var");
            self.eat_kw("let");
            let init = if self.eat_punct(";") {
                None
            } else {
                let e = self.expr()?;
                self.expect(";")?;
                Some(e)
            };
            let cond = if self.eat_punct(";") {
                None
            } else {
                let e = self.expr()?;
                self.expect(";")?;
                Some(e)
            };
            let step = if matches!(self.peek(), Tok::Punct(")")) { None } else { Some(self.expr()?) };
            self.expect(")")?;
            let body = self.stmt()?;
            return Ok(Stmt::For(init, cond, step, Box::new(body)));
        }
        // `var b=0` 當成一般賦值
        self.eat_kw("var");
        self.eat_kw("let");
        self.eat_kw("const");

        let e = self.expr()?;
        self.eat_punct(";");
        Ok(Stmt::Expr(e))
    }

    // ---- 運算式（含逗號） ----

    fn expr(&mut self) -> Result<Expr, String> {
        let first = self.assign()?;
        if !matches!(self.peek(), Tok::Punct(",")) {
            return Ok(first);
        }
        let mut list = vec![first];
        while self.eat_punct(",") {
            list.push(self.assign()?);
        }
        Ok(Expr::Comma(list))
    }

    /// 賦值層。右結合，所以 `b = a.n = 0` 會正確地先算最右邊。
    fn assign(&mut self) -> Result<Expr, String> {
        let lhs = self.cond()?;
        let op = match self.peek() {
            Tok::Punct("=") => Some(None),
            Tok::Punct("+=") => Some(Some(BinOp::Add)),
            Tok::Punct("-=") => Some(Some(BinOp::Sub)),
            Tok::Punct("*=") => Some(Some(BinOp::Mul)),
            Tok::Punct("/=") => Some(Some(BinOp::Div)),
            Tok::Punct("%=") => Some(Some(BinOp::Rem)),
            _ => None,
        };
        let Some(op) = op else { return Ok(lhs) };
        self.p += 1;
        let rhs = self.assign()?;
        let place = match lhs {
            Expr::Var(i) => Place::Var(i),
            Expr::Mega(i) => Place::Mega(i),
            Expr::GMega(i) => Place::GMega(i),
            other => return Err(format!("不能賦值給 {other:?}")),
        };
        Ok(Expr::Assign(place, op, Box::new(rhs)))
    }

    fn cond(&mut self) -> Result<Expr, String> {
        let c = self.binary(0)?;
        if !self.eat_punct("?") {
            return Ok(c);
        }
        let t = self.assign()?;
        self.expect(":")?;
        let f = self.assign()?;
        Ok(Expr::Cond(Box::new(c), Box::new(t), Box::new(f)))
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            let (op, prec) = match self.peek() {
                Tok::Punct("||") => (BinOp::Or, 1),
                Tok::Punct("&&") => (BinOp::And, 2),
                Tok::Punct("|") => (BinOp::BitOr, 3),
                Tok::Punct("^") => (BinOp::BitXor, 4),
                Tok::Punct("&") => (BinOp::BitAnd, 5),
                Tok::Punct("==") | Tok::Punct("===") => (BinOp::Eq, 6),
                Tok::Punct("!=") | Tok::Punct("!==") => (BinOp::Ne, 6),
                Tok::Punct("<") => (BinOp::Lt, 7),
                Tok::Punct(">") => (BinOp::Gt, 7),
                Tok::Punct("<=") => (BinOp::Le, 7),
                Tok::Punct(">=") => (BinOp::Ge, 7),
                Tok::Punct("<<") => (BinOp::Shl, 8),
                Tok::Punct(">>") => (BinOp::Shr, 8),
                Tok::Punct("+") => (BinOp::Add, 9),
                Tok::Punct("-") => (BinOp::Sub, 9),
                Tok::Punct("*") => (BinOp::Mul, 10),
                Tok::Punct("/") => (BinOp::Div, 10),
                Tok::Punct("%") => (BinOp::Rem, 10),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.p += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat_punct("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_punct("+") {
            return self.unary();
        }
        if self.eat_punct("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat_punct("~") {
            // 位元反相在這批 preset 裡沒出現過，但語法上允許
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        // 前置 ++／-- 一律當成 += 1
        if self.eat_punct("++") || self.eat_punct("--") {
            let inner = self.unary()?;
            if let Expr::Var(i) = inner {
                return Ok(Expr::Assign(Place::Var(i), Some(BinOp::Add), Box::new(Expr::Num(1.0))));
            }
            return Ok(inner);
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let e = self.primary()?;
        if let Expr::Var(i) = e {
            if self.eat_punct("++") {
                return Ok(Expr::Post(i, 1.0));
            }
            if self.eat_punct("--") {
                return Ok(Expr::Post(i, -1.0));
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.p += 1;
                Ok(Expr::Num(n))
            }
            Tok::Punct("(") => {
                self.p += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Ident(name) => {
                self.p += 1;
                // Math.xxx(...)
                if name == "Math" {
                    self.expect(".")?;
                    let Tok::Ident(f) = self.peek().clone() else {
                        return Err("Math. 後面不是識別字".into());
                    };
                    self.p += 1;
                    // Math.PI 之類的常數
                    if !matches!(self.peek(), Tok::Punct("(")) {
                        return Ok(Expr::Num(match f.as_str() {
                            "PI" => std::f64::consts::PI,
                            "E" => std::f64::consts::E,
                            "LN2" => std::f64::consts::LN_2,
                            "LN10" => std::f64::consts::LN_10,
                            "SQRT2" => std::f64::consts::SQRT_2,
                            _ => 0.0,
                        }));
                    }
                    let args = self.args()?;
                    let fun = math_fun(&f).ok_or_else(|| format!("不支援 Math.{f}"))?;
                    return Ok(Expr::Call(fun, args));
                }
                // 物件成員：a.name / a["name"] / a.megabuf[i]
                if matches!(self.peek(), Tok::Punct(".") | Tok::Punct("[")) && name.len() <= 2 {
                    return self.member(&name);
                }
                // 內建函式呼叫
                if matches!(self.peek(), Tok::Punct("(")) {
                    if let Some(fun) = helper_fun(&name) {
                        let args = self.args()?;
                        return Ok(Expr::Call(fun, args));
                    }
                    return Err(format!("不認識的函式 {name}"));
                }
                // 區域變數（迴圈計數器之類）。前綴 $ 避免跟 a.xxx 撞名。
                Ok(Expr::Var(self.vars.slot(&format!("${name}"))))
            }
            t => Err(format!("看不懂的 token {t:?}")),
        }
    }

    /// `a.foo`、`a["foo"]`、`a.megabuf[i]`、`a.gmegabuf[i]`
    fn member(&mut self, _obj: &str) -> Result<Expr, String> {
        let key = if self.eat_punct(".") {
            match self.peek().clone() {
                Tok::Ident(k) => {
                    self.p += 1;
                    k
                }
                t => return Err(format!(". 後面不是識別字：{t:?}")),
            }
        } else {
            self.expect("[")?;
            let k = match self.peek().clone() {
                Tok::Str(s) => {
                    self.p += 1;
                    s
                }
                t => return Err(format!("[ 後面不是字串：{t:?}")),
            };
            self.expect("]")?;
            k
        };

        if key == "megabuf" || key == "gmegabuf" {
            self.expect("[")?;
            let idx = self.expr()?;
            self.expect("]")?;
            return Ok(if key == "megabuf" {
                Expr::Mega(Box::new(idx))
            } else {
                Expr::GMega(Box::new(idx))
            });
        }
        Ok(Expr::Var(self.vars.slot(&key)))
    }

    fn args(&mut self) -> Result<Vec<Expr>, String> {
        self.expect("(")?;
        let mut out = Vec::new();
        if self.eat_punct(")") {
            return Ok(out);
        }
        loop {
            // 參數之間的逗號不是逗號運算子，所以只能解析到賦值層
            out.push(self.assign()?);
            if self.eat_punct(",") {
                continue;
            }
            self.expect(")")?;
            break;
        }
        Ok(out)
    }
}

fn math_fun(name: &str) -> Option<Fun> {
    Some(match name {
        "sin" => Fun::Sin,
        "cos" => Fun::Cos,
        "tan" => Fun::Tan,
        "asin" => Fun::Asin,
        "acos" => Fun::Acos,
        "atan" => Fun::Atan,
        "atan2" => Fun::Atan2,
        "abs" => Fun::Abs,
        "floor" => Fun::Floor,
        "ceil" => Fun::Ceil,
        "min" => Fun::Min,
        "max" => Fun::Max,
        "log" => Fun::Log,
        "log10" => Fun::Log10,
        "exp" => Fun::Exp,
        "pow" => Fun::Pow,
        "sqrt" => Fun::Sqrt,
        "random" => Fun::Random,
        "round" => Fun::Floor,
        _ => return None,
    })
}

fn helper_fun(name: &str) -> Option<Fun> {
    Some(match name {
        "div" => Fun::Div,
        "above" => Fun::Above,
        "below" => Fun::Below,
        "equal" => Fun::Equal,
        "bnot" => Fun::Bnot,
        "mod" => Fun::Mod,
        "pow" => Fun::Pow,
        "sqrt" => Fun::Sqrt,
        "sqr" => Fun::Sqr,
        "sign" => Fun::Sign,
        "sigmoid" => Fun::Sigmoid,
        "bitand" => Fun::BitAnd,
        "band" => Fun::Band,
        "bor" => Fun::Bor,
        "randint" => Fun::RandInt,
        "rand" => Fun::Rand,
        "int" => Fun::Int,
        "invsqrt" => Fun::Invsqrt,
        "abs" => Fun::Abs,
        "min" => Fun::Min,
        "max" => Fun::Max,
        "sin" => Fun::Sin,
        "cos" => Fun::Cos,
        "tan" => Fun::Tan,
        "log" => Fun::Log,
        "exp" => Fun::Exp,
        "floor" => Fun::Floor,
        _ => return None,
    })
}

// ---------------------------------------------------------------- 執行

/// EEL 的「真」判定：絕對值大於 1e-5。0.0000001 不算真。
#[inline]
fn truthy(v: f64) -> bool {
    v.abs() > 1e-5
}

pub struct Vm {
    pub vars: Vec<f64>,
    pub megabuf: Vec<f64>,
    pub gmegabuf: Vec<f64>,
    rng: u64,
    /// 防呆：迴圈總次數上限，免得壞掉的 preset 把整個程式卡死。
    budget: u64,
}

/// megabuf 在 MilkDrop 裡是 1M 個 slot。一開始不配那麼大，用到才長。
const MEGA_MAX: usize = 1 << 20;
const MEGA_INIT: usize = 1 << 12;
/// 單次執行的迴圈步數上限。最兇的 preset 初始化要清 1M 個 slot 兩次。
const BUDGET: u64 = 8_000_000;

impl Vm {
    pub fn new(n_vars: usize) -> Self {
        Self {
            vars: vec![0.0; n_vars],
            megabuf: vec![0.0; MEGA_INIT],
            gmegabuf: vec![0.0; MEGA_INIT],
            rng: 0x2545_F491_4F6C_DD1D,
            budget: BUDGET,
        }
    }

    pub fn ensure_vars(&mut self, n: usize) {
        if self.vars.len() < n {
            self.vars.resize(n, 0.0);
        }
    }

    pub fn get(&self, slot: usize) -> f64 {
        self.vars.get(slot).copied().unwrap_or(0.0)
    }

    pub fn set(&mut self, slot: usize, v: f64) {
        if slot >= self.vars.len() {
            self.vars.resize(slot + 1, 0.0);
        }
        self.vars[slot] = v;
    }

    fn rand(&mut self, max: f64) -> f64 {
        // xorshift64*，不需要密碼學強度，只要每次不一樣
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64
            / (1u64 << 53) as f64;
        x * max
    }

    fn buf_mut(&mut self, global: bool, idx: f64) -> Option<&mut f64> {
        let i = idx.floor();
        if !(0.0..MEGA_MAX as f64).contains(&i) || i.is_nan() {
            return None;
        }
        let i = i as usize;
        let buf = if global { &mut self.gmegabuf } else { &mut self.megabuf };
        if i >= buf.len() {
            let n = (i + 1).next_power_of_two().min(MEGA_MAX);
            buf.resize(n, 0.0);
        }
        buf.get_mut(i)
    }

    fn buf_get(&self, global: bool, idx: f64) -> f64 {
        let i = idx.floor();
        if !(0.0..MEGA_MAX as f64).contains(&i) || i.is_nan() {
            return 0.0;
        }
        let buf = if global { &self.gmegabuf } else { &self.megabuf };
        buf.get(i as usize).copied().unwrap_or(0.0)
    }

    pub fn run(&mut self, prog: &[Stmt]) {
        self.budget = BUDGET;
        for s in prog {
            if self.budget == 0 {
                break;
            }
            self.exec(s);
        }
    }

    fn exec(&mut self, s: &Stmt) {
        if self.budget == 0 {
            return;
        }
        match s {
            Stmt::Empty => {}
            Stmt::Expr(e) => {
                self.eval(e);
            }
            Stmt::Block(b) => {
                for s in b {
                    if self.budget == 0 {
                        return;
                    }
                    self.exec(s);
                }
            }
            Stmt::If(c, t, e) => {
                if truthy(self.eval(c)) {
                    self.exec(t);
                } else if let Some(e) = e {
                    self.exec(e);
                }
            }
            Stmt::While(c, b) => {
                while truthy(self.eval(c)) {
                    self.budget = self.budget.saturating_sub(1);
                    if self.budget == 0 {
                        return;
                    }
                    self.exec(b);
                }
            }
            Stmt::DoWhile(b, c) => loop {
                self.budget = self.budget.saturating_sub(1);
                if self.budget == 0 {
                    return;
                }
                self.exec(b);
                if !truthy(self.eval(c)) {
                    return;
                }
            },
            Stmt::For(init, cond, step, body) => {
                if let Some(i) = init {
                    self.eval(i);
                }
                loop {
                    if let Some(c) = cond
                        && !truthy(self.eval(c)) {
                            return;
                        }
                    self.budget = self.budget.saturating_sub(1);
                    if self.budget == 0 {
                        return;
                    }
                    self.exec(body);
                    if let Some(s) = step {
                        self.eval(s);
                    }
                }
            }
        }
    }

    pub fn eval(&mut self, e: &Expr) -> f64 {
        match e {
            Expr::Num(n) => *n,
            Expr::Var(i) => self.get(*i),
            Expr::Mega(i) => {
                let idx = self.eval(i);
                self.buf_get(false, idx)
            }
            Expr::GMega(i) => {
                let idx = self.eval(i);
                self.buf_get(true, idx)
            }
            Expr::Neg(a) => -self.eval(a),
            Expr::Not(a) => {
                if truthy(self.eval(a)) {
                    0.0
                } else {
                    1.0
                }
            }
            Expr::Bin(op, a, b) => self.binop(*op, a, b),
            Expr::Cond(c, t, f) => {
                if truthy(self.eval(c)) {
                    self.eval(t)
                } else {
                    self.eval(f)
                }
            }
            Expr::Post(slot, d) => {
                let old = self.get(*slot);
                self.set(*slot, old + d);
                old
            }
            Expr::Comma(list) => {
                let mut last = 0.0;
                for e in list {
                    last = self.eval(e);
                }
                last
            }
            Expr::Assign(place, op, rhs) => {
                let v = self.eval(rhs);
                match place {
                    Place::Var(i) => {
                        let nv = match op {
                            None => v,
                            Some(o) => apply(*o, self.get(*i), v),
                        };
                        self.set(*i, nv);
                        nv
                    }
                    Place::Mega(idx) | Place::GMega(idx) => {
                        let global = matches!(place, Place::GMega(_));
                        let i = self.eval(idx);
                        let cur = self.buf_get(global, i);
                        let nv = match op {
                            None => v,
                            Some(o) => apply(*o, cur, v),
                        };
                        if let Some(slot) = self.buf_mut(global, i) {
                            *slot = nv;
                        }
                        nv
                    }
                }
            }
            Expr::Call(f, args) => self.call(*f, args),
        }
    }

    fn binop(&mut self, op: BinOp, a: &Expr, b: &Expr) -> f64 {
        // && 和 || 要短路，不能先把兩邊都算出來
        match op {
            BinOp::And => {
                return if truthy(self.eval(a)) && truthy(self.eval(b)) { 1.0 } else { 0.0 }
            }
            BinOp::Or => {
                return if truthy(self.eval(a)) || truthy(self.eval(b)) { 1.0 } else { 0.0 }
            }
            _ => {}
        }
        let x = self.eval(a);
        let y = self.eval(b);
        apply(op, x, y)
    }

    fn call(&mut self, f: Fun, args: &[Expr]) -> f64 {
        let a = |vm: &mut Self, i: usize| -> f64 { args.get(i).map(|e| vm.eval(e)).unwrap_or(0.0) };
        match f {
            Fun::Random => self.rand(1.0),
            Fun::Rand => {
                let n = a(self, 0);
                self.rand(n)
            }
            Fun::RandInt => {
                let n = a(self, 0);
                self.rand(n).floor()
            }
            _ => {
                let x = a(self, 0);
                let y = a(self, 1);
                match f {
                    Fun::Sin => x.sin(),
                    Fun::Cos => x.cos(),
                    Fun::Tan => x.tan(),
                    Fun::Asin => x.clamp(-1.0, 1.0).asin(),
                    Fun::Acos => x.clamp(-1.0, 1.0).acos(),
                    Fun::Atan => x.atan(),
                    Fun::Atan2 => x.atan2(y),
                    Fun::Abs => x.abs(),
                    Fun::Floor | Fun::Int => x.floor(),
                    Fun::Ceil => x.ceil(),
                    Fun::Min => x.min(y),
                    Fun::Max => x.max(y),
                    Fun::Log => x.ln(),
                    Fun::Log10 => x.log10(),
                    Fun::Exp => x.exp(),
                    Fun::Pow => x.powf(y),
                    Fun::Sqrt => x.abs().sqrt(),
                    Fun::Invsqrt => {
                        let s = x.abs().sqrt();
                        if s > 1e-9 { 1.0 / s } else { 0.0 }
                    }
                    Fun::Sqr => x * x,
                    // 以下全部照抄 butterchurn 的執行期定義，語意不能自己發明
                    Fun::Div => {
                        if y == 0.0 { 0.0 } else { x / y }
                    }
                    Fun::Above => {
                        if x > y { 1.0 } else { 0.0 }
                    }
                    Fun::Below => {
                        if x < y { 1.0 } else { 0.0 }
                    }
                    Fun::Equal => {
                        if (x - y).abs() < 1e-5 { 1.0 } else { 0.0 }
                    }
                    Fun::Bnot => {
                        if x.abs() < 1e-5 { 1.0 } else { 0.0 }
                    }
                    Fun::Mod => {
                        let yi = y.floor();
                        if yi == 0.0 { 0.0 } else { (x.floor() as i64 % yi as i64) as f64 }
                    }
                    Fun::Sign => {
                        if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }
                    }
                    Fun::Sigmoid => {
                        let d = 1.0 + (-x * y).exp();
                        if d.abs() > 1e-5 { 1.0 / d } else { 0.0 }
                    }
                    Fun::BitAnd => ((x.floor() as i64) & (y.floor() as i64)) as f64,
                    Fun::Band => {
                        if x.abs() > 1e-5 && y.abs() > 1e-5 { 1.0 } else { 0.0 }
                    }
                    Fun::Bor
                        if (x.abs() > 1e-5 || y.abs() > 1e-5) => { 1.0 }
                    _ => 0.0,
                }
            }
        }
    }
}

fn apply(op: BinOp, x: f64, y: f64) -> f64 {
    match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        // JS 的 / 遇到 0 會給 Infinity，但 MilkDrop 的世界裡那會讓整個畫面炸掉。
        // butterchurn 自己也是用 div() 包起來的，這裡對裸除法做同樣的保護。
        BinOp::Div => {
            if y == 0.0 { 0.0 } else { x / y }
        }
        BinOp::Rem => {
            if y == 0.0 { 0.0 } else { x % y }
        }
        BinOp::Lt => bool_f(x < y),
        BinOp::Gt => bool_f(x > y),
        BinOp::Le => bool_f(x <= y),
        BinOp::Ge => bool_f(x >= y),
        BinOp::Eq => bool_f((x - y).abs() < 1e-9),
        BinOp::Ne => bool_f((x - y).abs() >= 1e-9),
        BinOp::And => bool_f(truthy(x) && truthy(y)),
        BinOp::Or => bool_f(truthy(x) || truthy(y)),
        BinOp::BitAnd => ((x.floor() as i64) & (y.floor() as i64)) as f64,
        BinOp::BitOr => ((x.floor() as i64) | (y.floor() as i64)) as f64,
        BinOp::BitXor => ((x.floor() as i64) ^ (y.floor() as i64)) as f64,
        BinOp::Shl => (((x.floor() as i64) << ((y.floor() as i64) & 63)) as f64).clamp(-1e18, 1e18),
        BinOp::Shr => ((x.floor() as i64) >> ((y.floor() as i64) & 63)) as f64,
    }
}

#[inline]
fn bool_f(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

// ---------------------------------------------------------------- 作用域

/// 找出一段程式會**寫到**哪些變數槽位。
///
/// MilkDrop 的 custom wave 與 custom shape 各自有獨立的變數作用域：
/// 波形裡寫的 `t1`、`x`、`sample` 不會污染 preset 的 frame_eqs。
/// 這邊所有方程式共用一張變數表（因為 init→frame→pixel 本來就要互通），
/// 所以得靠這個把「這段自己的變數」框出來，跑完再換回去。
pub fn assigned_slots(prog: &[Stmt]) -> Vec<usize> {
    let mut out = Vec::new();
    for s in prog {
        walk_stmt(s, &mut out);
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn walk_stmt(s: &Stmt, out: &mut Vec<usize>) {
    match s {
        Stmt::Expr(e) => walk_expr(e, out),
        Stmt::Block(v) => v.iter().for_each(|s| walk_stmt(s, out)),
        Stmt::If(c, a, b) => {
            walk_expr(c, out);
            walk_stmt(a, out);
            if let Some(b) = b {
                walk_stmt(b, out);
            }
        }
        Stmt::While(c, b) => {
            walk_expr(c, out);
            walk_stmt(b, out);
        }
        Stmt::DoWhile(b, c) => {
            walk_stmt(b, out);
            walk_expr(c, out);
        }
        Stmt::For(a, b, c, d) => {
            for e in [a, b, c].into_iter().flatten() {
                walk_expr(e, out);
            }
            walk_stmt(d, out);
        }
        Stmt::Empty => {}
    }
}

fn walk_expr(e: &Expr, out: &mut Vec<usize>) {
    match e {
        Expr::Num(_) | Expr::Var(_) => {}
        Expr::Mega(a) | Expr::GMega(a) | Expr::Neg(a) | Expr::Not(a) => walk_expr(a, out),
        Expr::Bin(_, a, b) => {
            walk_expr(a, out);
            walk_expr(b, out);
        }
        Expr::Cond(a, b, c) => {
            walk_expr(a, out);
            walk_expr(b, out);
            walk_expr(c, out);
        }
        Expr::Assign(p, _, v) => {
            match p {
                Place::Var(s) => out.push(*s),
                Place::Mega(i) | Place::GMega(i) => walk_expr(i, out),
            }
            walk_expr(v, out);
        }
        Expr::Post(s, _) => out.push(*s),
        Expr::Call(_, args) => args.iter().for_each(|a| walk_expr(a, out)),
        Expr::Comma(v) => v.iter().for_each(|a| walk_expr(a, out)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> (Vm, Vars) {
        let mut vars = Vars::default();
        let prog = parse(src, &mut vars).expect("解析失敗");
        let mut vm = Vm::new(vars.len());
        vm.run(&prog);
        (vm, vars)
    }

    fn val(src: &str, name: &str) -> f64 {
        let (vm, vars) = run(src);
        vm.get(vars.find(name).unwrap_or_else(|| panic!("找不到變數 {name}")))
    }

    #[test]
    fn 基本賦值與算術() {
        assert_eq!(val("a.x=1+2*3;", "x"), 7.0);
        assert_eq!(val("a.x=(1+2)*3;", "x"), 9.0);
        assert_eq!(val("a.x=.5;", "x"), 0.5);
        assert_eq!(val("a.x=1E3;", "x"), 1000.0);
    }

    #[test]
    fn 鏈式賦值與逗號() {
        let (vm, vars) = run("b=a.n=5;a.m=b+1;");
        assert_eq!(vm.get(vars.find("n").unwrap()), 5.0);
        assert_eq!(vm.get(vars.find("m").unwrap()), 6.0);
    }

    #[test]
    fn 除以零回零而不是無限大() {
        assert_eq!(val("a.x=div(1,0);", "x"), 0.0);
        assert_eq!(val("a.x=1/0;", "x"), 0.0);
    }

    #[test]
    fn helper函式語意與butterchurn一致() {
        assert_eq!(val("a.x=above(2,1);", "x"), 1.0);
        assert_eq!(val("a.x=below(2,1);", "x"), 0.0);
        // equal 是 1e-5 的容差，不是精確相等
        assert_eq!(val("a.x=equal(1,1.000001);", "x"), 1.0);
        assert_eq!(val("a.x=bnot(0.000001);", "x"), 1.0);
        assert_eq!(val("a.x=bnot(1);", "x"), 0.0);
        assert_eq!(val("a.x=mod(7,3);", "x"), 1.0);
        assert_eq!(val("a.x=mod(7,0);", "x"), 0.0);
        assert_eq!(val("a.x=sign(-3);", "x"), -1.0);
        assert_eq!(val("a.x=sqr(4);", "x"), 16.0);
        assert_eq!(val("a.x=band(1,0);", "x"), 0.0);
        assert_eq!(val("a.x=bor(1,0);", "x"), 1.0);
        assert_eq!(val("a.x=bitand(6,3);", "x"), 2.0);
    }

    #[test]
    fn 三元與短路() {
        assert_eq!(val("a.x=1>0?10:20;", "x"), 10.0);
        assert_eq!(val("a.x=0?10:20;", "x"), 20.0);
        // 0.000001 在 EEL 的世界裡是假
        assert_eq!(val("a.x=0.000001?10:20;", "x"), 20.0);
    }

    #[test]
    fn 迴圈() {
        assert_eq!(val("a.s=0;for(var b=0;5>b;b++)a.s+=b;", "s"), 10.0);
        assert_eq!(val("a.i=0;a.s=0;while(3>a.i){a.s+=a.i;a.i+=1}", "s"), 3.0);
        assert_eq!(val("a.i=0;do{a.i+=1}while(3>a.i);", "i"), 3.0);
    }

    #[test]
    fn megabuf() {
        assert_eq!(val("a.megabuf[3]=42;a.x=a.megabuf[3];", "x"), 42.0);
        // 越界讀取回 0，不該 panic
        assert_eq!(val("a.x=a.megabuf[99999999];", "x"), 0.0);
        assert_eq!(val("a.x=a.gmegabuf[-5];", "x"), 0.0);
    }

    #[test]
    fn 保留字要用中括號存取() {
        assert_eq!(val("a[\"var\"]=7;a.x=a[\"var\"];", "x"), 7.0);
    }

    #[test]
    fn 後置遞增回傳舊值() {
        let (vm, vars) = run("b=0;a.x=b++;a.y=b;");
        assert_eq!(vm.get(vars.find("x").unwrap()), 0.0);
        assert_eq!(vm.get(vars.find("y").unwrap()), 1.0);
    }

    #[test]
    fn 無窮迴圈會被預算擋下來而不是卡死() {
        let mut vars = Vars::default();
        let prog = parse("a.i=0;while(1){a.i+=1}", &mut vars).unwrap();
        let mut vm = Vm::new(vars.len());
        vm.run(&prog); // 不該永遠不回來
        assert!(vm.get(vars.find("i").unwrap()) > 0.0);
    }
}
