//! `crystal-tools import sapphire-quests <dir>`: Sapphire's quest scripts (C++, `src/scripts/quest`)
//! as quest definitions (`crystal_common::quest`), for the quests the β has. Sapphire's scripts are
//! the only source left of which talk plays which scene and what a scene's result does (Square's
//! server scripts never shipped); they are imported as written, tagged `sapphire`, and what this
//! format cannot express goes to the definition's `review`, verbatim.
//!
//! The scripts are regular (generated, then edited): constants (`static constexpr auto Actor0 =
//! 1001148;`, `enum Sequence { Seq0 = 0, ... }`), `onTalk` picking a scene by actor, sequence and
//! variables, `SceneNNNNN` playing scene N with `bindSceneReturn( &X::SceneNNNNNReturn )`, and the
//! returns acting on the result. A small statement parser reads exactly that much C++.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crystal_common::quest::{Action, Cond, Op, QuestDef, Rule, Source, Talk, Var};

// ---- tokens ------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Punct(&'static str),
}

const PUNCTS: [&str; 28] = [
    "::", "->", "==", "!=", "<=", ">=", "&&", "||", "++", "+=", "(", ")", "{", "}", "[", "]", ";", ",", "<", ">", "=", "!",
    "&", "|", ":", ".", "+", "-",
];

fn tokenize(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") || src[i..].starts_with('#') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
        } else if c == '"' || c == '\'' {
            let q = b[i];
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != q {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            out.push(Tok::Str(src[start..i.min(b.len())].to_owned()));
            i += 1;
        } else if c.is_ascii_digit() || (c == '-' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit() && prev_is_operand_start(&out)) {
            let start = i;
            i += 1;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            let t = src[start..i].trim_end_matches(['f', 'F', 'u', 'U', 'l', 'L']);
            let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).map(|v| v as f64).unwrap_or(0.0)
            } else {
                t.parse().unwrap_or(0.0)
            };
            out.push(Tok::Num(v));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_owned()));
        } else if let Some(p) = PUNCTS.iter().find(|p| src[i..].starts_with(**p)) {
            out.push(Tok::Punct(p));
            i += p.len();
        } else {
            i += 1; // anything else (operators this reader never needs)
        }
    }
    out
}

fn prev_is_operand_start(out: &[Tok]) -> bool {
    !matches!(out.last(), Some(Tok::Ident(_) | Tok::Num(_) | Tok::Punct(")")))
}

// ---- expressions and statements ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Num(f64),
    Ident(String),
    /// `a.b`, `a->b`, `a::b`
    Path(Box<Expr>, String),
    Call(Box<Expr>, Vec<Expr>),
    Not(Box<Expr>),
    Bin(Box<Expr>, &'static str, Box<Expr>),
    /// `{ x, y, z }`
    List(Vec<Expr>),
    Other(String),
}

#[derive(Debug, Clone)]
enum Stmt {
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    Switch(Expr, Vec<(Vec<Expr>, Vec<Stmt>)>),
    Expr(Expr),
    Return,
}

struct P<'a> {
    t: &'a [Tok],
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if *x == p)
    }
    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == s)
    }
    fn eat(&mut self, p: &str) -> bool {
        let y = self.is(p);
        if y {
            self.i += 1;
        }
        y
    }

    fn block(&mut self) -> Vec<Stmt> {
        let mut out = Vec::new();
        if !self.eat("{") {
            if let Some(s) = self.stmt() {
                out.push(s);
            }
            return out;
        }
        while self.peek().is_some() && !self.eat("}") {
            if let Some(s) = self.stmt() {
                out.push(s);
            }
        }
        out
    }

    fn stmt(&mut self) -> Option<Stmt> {
        if self.is("{") {
            let b = self.block();
            return Some(Stmt::If(Expr::Num(1.0), b, vec![]));
        }
        if self.is_ident("if") {
            self.i += 1;
            self.eat("(");
            let cond = self.expr();
            self.eat(")");
            let then = self.block();
            let otherwise = if self.is_ident("else") {
                self.i += 1;
                self.block()
            } else {
                vec![]
            };
            return Some(Stmt::If(cond, then, otherwise));
        }
        if self.is_ident("switch") {
            self.i += 1;
            self.eat("(");
            let subject = self.expr();
            self.eat(")");
            self.eat("{");
            let mut cases: Vec<(Vec<Expr>, Vec<Stmt>)> = Vec::new();
            while self.peek().is_some() && !self.eat("}") {
                if self.is_ident("case") {
                    self.i += 1;
                    let label = self.expr();
                    self.eat(":");
                    match cases.last_mut() {
                        Some((labels, body)) if body.is_empty() => labels.push(label),
                        _ => cases.push((vec![label], vec![])),
                    }
                } else if self.is_ident("default") {
                    self.i += 1;
                    self.eat(":");
                    cases.push((vec![Expr::Other("default".into())], vec![]));
                } else if self.is_ident("break") {
                    self.i += 1;
                    self.eat(";");
                    cases.push((vec![], vec![])); // a break ends the case
                } else if let Some(s) = self.stmt()
                    && let Some((_, body)) = cases.last_mut()
                {
                    body.push(s);
                }
            }
            cases.retain(|(labels, _)| !labels.is_empty());
            return Some(Stmt::Switch(subject, cases));
        }
        if self.is_ident("return") {
            while self.peek().is_some() && !self.eat(";") {
                self.i += 1;
            }
            return Some(Stmt::Return);
        }
        if self.is_ident("break") {
            self.i += 1;
            self.eat(";");
            return None;
        }
        // a declaration or an expression, to its ';'
        let start = self.i;
        let e = self.expr();
        if !self.eat(";") {
            while self.peek().is_some() && !self.is(";") && !self.is("}") {
                self.i += 1;
            }
            self.eat(";");
            return Some(Stmt::Expr(Expr::Other(text(&self.t[start..self.i]))));
        }
        Some(Stmt::Expr(e))
    }

    fn expr(&mut self) -> Expr {
        self.binary(0)
    }

    fn binary(&mut self, level: usize) -> Expr {
        const LEVELS: [&[&str]; 4] = [&["||"], &["&&"], &["==", "!=", "<", "<=", ">", ">="], &["+", "-"]];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1);
        while let Some(Tok::Punct(p)) = self.peek().cloned() {
            if !LEVELS[level].contains(&p) {
                break;
            }
            self.i += 1;
            let rhs = self.binary(level + 1);
            lhs = Expr::Bin(Box::new(lhs), p, Box::new(rhs));
        }
        lhs
    }

    fn unary(&mut self) -> Expr {
        if self.eat("!") {
            return Expr::Not(Box::new(self.unary()));
        }
        if self.eat("&") {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Expr {
        let mut e = match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Expr::Num(n)
            }
            Some(Tok::Ident(s)) => {
                self.i += 1;
                Expr::Ident(s)
            }
            Some(Tok::Str(s)) => {
                self.i += 1;
                Expr::Other(format!("\"{s}\""))
            }
            Some(Tok::Punct("(")) => {
                self.i += 1;
                let e = self.expr();
                self.eat(")");
                e
            }
            Some(Tok::Punct("{")) => {
                self.i += 1;
                let mut items = Vec::new();
                while self.peek().is_some() && !self.eat("}") {
                    items.push(self.expr());
                    self.eat(",");
                }
                Expr::List(items)
            }
            _ => {
                self.i += 1;
                Expr::Other(String::new())
            }
        };
        loop {
            if self.eat(".") || self.eat("->") || self.eat("::") {
                if let Some(Tok::Ident(s)) = self.peek().cloned() {
                    self.i += 1;
                    e = Expr::Path(Box::new(e), s);
                }
            } else if self.is("(") {
                self.i += 1;
                let mut args = Vec::new();
                while self.peek().is_some() && !self.eat(")") {
                    args.push(self.expr());
                    self.eat(",");
                }
                e = Expr::Call(Box::new(e), args);
            } else if self.is("<") && matches!(e, Expr::Ident(_) | Expr::Path(..)) && self.template_ahead() {
                // a template argument list (`getRow< Excel::Quest >`): skipped
                let mut depth = 0;
                while let Some(t) = self.peek() {
                    if *t == Tok::Punct("<") {
                        depth += 1;
                    } else if *t == Tok::Punct(">") {
                        depth -= 1;
                        if depth == 0 {
                            self.i += 1;
                            break;
                        }
                    }
                    self.i += 1;
                }
            } else {
                return e;
            }
        }
    }

    /// Whether a `<` here opens a template argument list (closed by `>` then `(` or `::`).
    fn template_ahead(&self) -> bool {
        let mut j = self.i + 1;
        while let Some(t) = self.t.get(j) {
            match t {
                Tok::Punct(">") => return matches!(self.t.get(j + 1), Some(Tok::Punct("(" | "::"))),
                Tok::Punct(";" | "(" | ")" | "==" | "&&" | "||") => return false,
                _ => j += 1,
            }
        }
        false
    }
}

fn text(t: &[Tok]) -> String {
    let mut s = String::new();
    for tok in t {
        match tok {
            Tok::Ident(x) => {
                if s.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                    s.push(' ');
                }
                s.push_str(x)
            }
            Tok::Num(n) => s.push_str(&n.to_string()),
            Tok::Str(x) => s.push_str(&format!("\"{x}\"")),
            Tok::Punct(p) => s.push_str(p),
        }
    }
    s
}

fn show(e: &Expr) -> String {
    match e {
        Expr::Num(n) => n.to_string(),
        Expr::Ident(s) => s.clone(),
        Expr::Path(a, b) => format!("{}.{b}", show(a)),
        Expr::Call(f, args) => format!("{}({})", show(f), args.iter().map(show).collect::<Vec<_>>().join(", ")),
        Expr::Not(a) => format!("!{}", show(a)),
        Expr::Bin(a, op, b) => format!("{} {op} {}", show(a), show(b)),
        Expr::List(v) => format!("{{{}}}", v.iter().map(show).collect::<Vec<_>>().join(", ")),
        Expr::Other(s) => s.clone(),
    }
}

// ---- a script --------------------------------------------------------------------------------------

struct ScriptSrc {
    quest: u32,
    consts: HashMap<String, f64>,
    methods: HashMap<String, Vec<Tok>>,
}

fn read_script(src: &str) -> Option<ScriptSrc> {
    let t = tokenize(src);
    let mut consts = HashMap::new();
    let mut methods = HashMap::new();
    let mut quest = None;
    let mut i = 0;
    while i < t.len() {
        // static constexpr auto Name = value;
        if let (Some(Tok::Ident(a)), Some(Tok::Ident(b))) = (t.get(i), t.get(i + 1))
            && a == "constexpr"
            && b == "auto"
            && let (Some(Tok::Ident(name)), Some(Tok::Punct("=")), Some(Tok::Num(v))) = (t.get(i + 2), t.get(i + 3), t.get(i + 4))
        {
            consts.insert(name.clone(), *v);
        }
        // enum entries: Name = value,
        if let (Some(Tok::Ident(name)), Some(Tok::Punct("=")), Some(Tok::Num(v)), Some(Tok::Punct("," | "}"))) =
            (t.get(i), t.get(i + 1), t.get(i + 2), t.get(i + 3))
        {
            consts.entry(name.clone()).or_insert(*v);
        }
        // QuestScript( 65575 )
        if let (Some(Tok::Ident(a)), Some(Tok::Punct("(")), Some(Tok::Num(v))) = (t.get(i), t.get(i + 1), t.get(i + 2))
            && a == "QuestScript"
        {
            quest = Some(*v as u32);
        }
        // void name( ... ) [override] { body }
        if let (Some(Tok::Ident(v)), Some(Tok::Ident(name)), Some(Tok::Punct("("))) = (t.get(i), t.get(i + 1), t.get(i + 2))
            && v == "void"
        {
            let mut j = i + 3;
            let mut depth = 1;
            while j < t.len() && depth > 0 {
                match t[j] {
                    Tok::Punct("(") => depth += 1,
                    Tok::Punct(")") => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            while j < t.len() && t[j] != Tok::Punct("{") && t[j] != Tok::Punct(";") {
                j += 1;
            }
            if t.get(j) == Some(&Tok::Punct("{")) {
                let start = j;
                let mut depth = 0;
                while j < t.len() {
                    match t[j] {
                        Tok::Punct("{") => depth += 1,
                        Tok::Punct("}") => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                methods.insert(name.clone(), t[start..=j.min(t.len() - 1)].to_vec());
                i = j;
            }
        }
        i += 1;
    }
    Some(ScriptSrc { quest: quest?, consts, methods })
}

fn body(script: &ScriptSrc, method: &str) -> Option<Vec<Stmt>> {
    let toks = script.methods.get(method)?;
    Some(P { t: toks, i: 0 }.block())
}

// ---- conversion ------------------------------------------------------------------------------------

struct Conv<'a> {
    s: &'a ScriptSrc,
    review: Vec<String>,
}

impl Conv<'_> {
    fn value(&self, e: &Expr) -> Option<f64> {
        match e {
            Expr::Num(n) => Some(*n),
            Expr::Ident(x) if x == "true" => Some(1.0),
            Expr::Ident(x) if x == "false" => Some(0.0),
            Expr::Ident(x) => self.s.consts.get(x).copied(),
            Expr::Path(_, x) => self.s.consts.get(x).copied(),
            _ => None,
        }
    }

    /// `a.method(args)` → (method, args), for `quest.`, `player.`, `result.`, `eventMgr().` ...
    fn call<'e>(&self, e: &'e Expr) -> Option<(&'e str, &'e [Expr])> {
        match e {
            Expr::Call(f, args) => match &**f {
                Expr::Path(_, m) | Expr::Ident(m) => Some((m.as_str(), args.as_slice())),
                _ => None,
            },
            _ => None,
        }
    }

    /// A condition as definition conditions (an `&&` of comparisons), or `None` when it says
    /// more than they can.
    fn cond(&self, e: &Expr) -> Option<Vec<Cond>> {
        match e {
            Expr::Bin(a, "&&", b) => {
                let mut v = self.cond(a)?;
                v.extend(self.cond(b)?);
                Some(v)
            }
            Expr::Num(n) if *n != 0.0 => Some(vec![]),
            Expr::Bin(a, op, b) => {
                let op = match *op {
                    "==" => Op::Eq,
                    "!=" => Op::Ne,
                    "<" => Op::Lt,
                    "<=" => Op::Le,
                    ">" => Op::Gt,
                    ">=" => Op::Ge,
                    _ => return None,
                };
                let v = self.value(b)?;
                let (m, args) = self.call(a)?;
                self.leaf(m, args, op, v)
            }
            Expr::Not(a) => {
                let (m, args) = self.call(a)?;
                self.leaf(m, args, Op::Eq, 0.0)
            }
            e => {
                let (m, args) = self.call(e)?;
                self.leaf(m, args, Op::Ne, 0.0)
            }
        }
    }

    fn leaf(&self, m: &str, args: &[Expr], op: Op, v: f64) -> Option<Vec<Cond>> {
        Some(vec![match m {
            "getSeq" => Cond::Seq { op, value: v as u8 },
            "getResult" => Cond::Result { index: self.value(args.first()?)? as u8, op, value: v as u32 },
            "getBitFlag8" => {
                let index = self.value(args.first()?)? as u8;
                let set = match op {
                    Op::Eq => v != 0.0,
                    Op::Ne => v == 0.0,
                    _ => return None,
                };
                Cond::Flag { index, set }
            }
            m => Cond::Var { var: Var::parse(m.strip_prefix("get")?)?, op, value: v as u8 },
        }])
    }

    fn negate(&self, conds: &[Cond]) -> Option<Vec<Cond>> {
        // only a single comparison negates into one condition
        let [c] = conds else { return None };
        let flip = |op: Op| match op {
            Op::Eq => Op::Ne,
            Op::Ne => Op::Eq,
            Op::Lt => Op::Ge,
            Op::Ge => Op::Lt,
            Op::Le => Op::Gt,
            Op::Gt => Op::Le,
        };
        Some(vec![match c.clone() {
            Cond::Seq { op, value } => Cond::Seq { op: flip(op), value },
            Cond::Result { index, op, value } => Cond::Result { index, op: flip(op), value },
            Cond::Var { var, op, value } => Cond::Var { var, op: flip(op), value },
            Cond::Flag { index, set } => Cond::Flag { index, set: !set },
        }])
    }

    /// Every path through `stmts` as (conditions, actions), in order.
    fn paths(&mut self, stmts: &[Stmt], depth: usize) -> Vec<(Vec<Cond>, Vec<Action>)> {
        let mut paths = vec![(vec![], vec![])];
        for st in stmts {
            match st {
                Stmt::Return => break,
                // a helper method of the script (checkQuestCompletion): its statements spliced in
                // here, branches and all
                Stmt::Expr(e)
                    if depth < 4
                        && self.call(e).is_some_and(|(m, _)| m.strip_prefix("Scene").is_none() && self.s.methods.contains_key(m)) =>
                {
                    let (m, _) = self.call(e).unwrap();
                    let inner = self.paths(&body(self.s, m).unwrap_or_default(), depth + 1);
                    paths = paths
                        .iter()
                        .flat_map(|(pc, pa)| {
                            inner.iter().map(move |(ic, ia)| ([pc.clone(), ic.clone()].concat(), [pa.clone(), ia.clone()].concat()))
                        })
                        .collect();
                }
                Stmt::Expr(e) => {
                    let acts = self.action(e);
                    for p in &mut paths {
                        p.1.extend(acts.clone());
                    }
                }
                Stmt::If(c, then, otherwise) => {
                    let conds = self.cond(c);
                    let neg = conds.as_deref().and_then(|c| self.negate(c));
                    if conds.is_none() || (!otherwise.is_empty() && neg.is_none()) {
                        self.review.push(format!("condition `{}` not expressible; its branch dropped", show(c)));
                    }
                    let mut next = Vec::new();
                    for (pc, pa) in &paths {
                        if let Some(c) = &conds {
                            for (bc, ba) in self.paths(then, depth) {
                                next.push(([pc.clone(), c.clone(), bc].concat(), [pa.clone(), ba].concat()));
                            }
                        }
                        let else_conds = if conds.as_deref() == Some(&[]) { None } else { neg.clone() };
                        match (&else_conds, otherwise.is_empty()) {
                            (Some(n), _) => {
                                for (bc, ba) in self.paths(otherwise, depth) {
                                    next.push(([pc.clone(), n.clone(), bc].concat(), [pa.clone(), ba].concat()));
                                }
                            }
                            (None, true) if conds.as_deref() != Some(&[]) => {
                                // no else and no single negation: the path that skips the branch
                                // keeps no extra condition (a later, more general rule)
                                next.push((pc.clone(), pa.clone()));
                            }
                            _ => {}
                        }
                    }
                    paths = next;
                }
                Stmt::Switch(subject, _) => {
                    self.review.push(format!("switch on `{}` in a scene return not expressible", show(subject)));
                }
            }
        }
        paths
    }

    fn action(&mut self, e: &Expr) -> Vec<Action> {
        let Some((m, args)) = self.call(e) else {
            if !matches!(e, Expr::Other(s) if s.is_empty()) {
                self.review.push(format!("statement `{}`", show(e)));
            }
            return vec![];
        };
        let arg = |i: usize| args.get(i).and_then(|a| self.value(a));
        let scene_of = |m: &str| m.strip_prefix("Scene").and_then(|n| n.parse::<u16>().ok());
        match m {
            m if scene_of(m).is_some() => vec![Action::Play { scene: scene_of(m).unwrap() }],
            "setSeq" => arg(0).map(|s| vec![Action::SetSeq { seq: s as u8 }]).unwrap_or_default(),
            "setBitFlag8" => match (arg(0), arg(1)) {
                (Some(i), Some(v)) => vec![Action::SetFlag { index: i as u8, set: v != 0.0 }],
                _ => self.unsupported(e),
            },
            "finishQuest" => {
                let choice = args.get(1).and_then(|a| match self.call(a) {
                    Some(("getResult", r)) => r.first().and_then(|x| self.value(x)).map(|v| v as u8),
                    _ => None,
                });
                vec![Action::Complete { choice }]
            }
            "setOpeningSequence" => arg(0).map(|s| vec![Action::OpeningSeq { seq: s as u8 }]).unwrap_or_default(),
            "eventFinish" => vec![Action::Finish],
            "eventStart" if args.iter().any(|a| self.value(a).is_some_and(|v| (0x130001..=0x130003).contains(&(v as u32)))) => vec![],
            "playQuestScene" if args.get(1).and_then(|a| self.value(a)).is_some_and(|v| (0x130001..=0x130003).contains(&(v as u32))) => {
                arg(2).map(|s| vec![Action::OpeningScene { scene: s as u16 }]).unwrap_or_default()
            }
            "requestMoveTerritory" => {
                let territory = args.get(2).and_then(|a| {
                    // teriMgr().getTerritoryByTypeId( 181 )->getGuId()
                    fn find(c: &Conv, e: &Expr) -> Option<f64> {
                        match e {
                            Expr::Call(f, a) => match &**f {
                                Expr::Path(_, m) if m == "getTerritoryByTypeId" => a.first().and_then(|x| c.value(x)),
                                Expr::Path(inner, _) => find(c, inner),
                                _ => None,
                            },
                            Expr::Path(inner, _) => find(c, inner),
                            _ => None,
                        }
                    }
                    find(self, a)
                });
                let pos = match args.get(3) {
                    Some(Expr::List(v)) if v.len() == 3 => v.iter().map(|x| self.value(x).map(|f| f as f32)).collect::<Option<Vec<_>>>(),
                    _ => None,
                };
                match (territory, pos, arg(4)) {
                    (Some(t), Some(p), Some(r)) => {
                        vec![Action::Warp { territory: t as u16, pos: [p[0], p[1], p[2]], rotation: r as f32 }]
                    }
                    _ => self.unsupported(e),
                }
            }
            m if m.starts_with("setUI8") => {
                let var = Var::parse(&m[3..]);
                // setUI8AL( quest.getUI8AL() + 1 ): an increment of the same variable
                let add = match args.first() {
                    Some(Expr::Bin(a, op @ ("+" | "-"), b)) if self.call(a).is_some_and(|(g, _)| g.strip_prefix("get") == Some(&m[3..])) => {
                        self.value(b).map(|n| if *op == "+" { n as i8 } else { -(n as i8) })
                    }
                    _ => None,
                };
                match (var, add, arg(0)) {
                    (Some(var), Some(delta), _) => vec![Action::AddVar { var, delta }],
                    (Some(var), None, Some(v)) => vec![Action::SetVar { var, value: v as u8 }],
                    _ => self.unsupported(e),
                }
            }
            // the client's quest progress notice: kept, the engine sends it once its packet is known
            "sendEventNotice" => {
                let values: Option<Vec<u32>> = args.iter().skip(2).map(|a| self.value(a).map(|v| v as u32)).collect();
                match values {
                    Some(v) if !v.is_empty() => vec![Action::Notice { index: v[0] as u8, values: v[1..].to_vec() }],
                    _ => self.unsupported(e),
                }
            }
            // Sapphire's own bookkeeping, not the quest's flow: a gearset for a newly unlocked
            // class (its UI), debug messages
            "addGearSet" | "sendDebug" => vec![],
            _ => self.unsupported(e),
        }
    }

    fn unsupported(&mut self, e: &Expr) -> Vec<Action> {
        self.review.push(format!("statement `{}`", show(e)));
        vec![]
    }

    /// onTalk: talk rules, by actor.
    fn talks(&mut self, stmts: &[Stmt], actor: Option<u32>, when: Vec<Cond>, out: &mut Vec<Talk>) {
        for st in stmts {
            match st {
                Stmt::Switch(subject, cases) if show(subject) == "actorId" => {
                    for (labels, body) in cases {
                        for l in labels {
                            match self.value(l) {
                                Some(a) => self.talks(body, Some(a as u32), when.clone(), out),
                                None => self.review.push(format!("onTalk case `{}`", show(l))),
                            }
                        }
                    }
                }
                Stmt::If(Expr::Bin(a, "==", b), then, otherwise) if show(a) == "actorId" => {
                    match self.value(b) {
                        Some(x) => self.talks(then, Some(x as u32), when.clone(), out),
                        None => self.review.push(format!("onTalk actor `{}`", show(b))),
                    }
                    self.talks(otherwise, actor, when.clone(), out);
                }
                Stmt::If(c, then, otherwise) => match self.cond(c) {
                    Some(conds) => {
                        self.talks(then, actor, [when.clone(), conds.clone()].concat(), out);
                        if !otherwise.is_empty() {
                            match self.negate(&conds) {
                                Some(n) => self.talks(otherwise, actor, [when.clone(), n].concat(), out),
                                None => self.review.push(format!("onTalk else of `{}` not expressible", show(c))),
                            }
                        }
                    }
                    None => self.review.push(format!("onTalk condition `{}` not expressible", show(c))),
                },
                // eventActionStart( player, getId(), EventActionSearch, [..]( .. ) { SceneN( .. ); }, .. ):
                // the action's cast bar, then the scene its completion plays
                Stmt::Expr(e) if self.call(e).is_some_and(|(m, _)| m == "eventActionStart") => {
                    let (_, args) = self.call(e).unwrap();
                    let action = args.get(2).and_then(|a| self.value(a)).map(|v| v as u32);
                    fn scene_in(e: &Expr) -> Option<u16> {
                        match e {
                            Expr::Call(f, args) => match &**f {
                                Expr::Ident(m) if m.starts_with("Scene") => m[5..].parse().ok(),
                                _ => args.iter().find_map(scene_in),
                            },
                            Expr::List(items) => items.iter().find_map(scene_in),
                            _ => None,
                        }
                    }
                    match (args.iter().find_map(scene_in), actor, action) {
                        (Some(scene), Some(actor), Some(action)) => {
                            out.push(Talk { actor, when: when.clone(), scene, action: Some(action) })
                        }
                        _ => self.review.push(format!("onTalk event action `{}`", show(e))),
                    }
                }
                Stmt::Expr(e) => {
                    let scene = self.call(e).and_then(|(m, _)| m.strip_prefix("Scene")?.parse::<u16>().ok());
                    match (scene, actor) {
                        (Some(scene), Some(actor)) => out.push(Talk { actor, when: when.clone(), scene, action: None }),
                        (Some(scene), None) => self.review.push(format!("onTalk plays scene {scene} for any actor")),
                        _ => self.review.push(format!("onTalk statement `{}`", show(e))),
                    }
                }
                Stmt::Switch(subject, _) => self.review.push(format!("onTalk switch on `{}`", show(subject))),
                Stmt::Return => break,
            }
        }
    }
}

/// One script as a definition (`script` is the β's name for it, `Quest` col 1).
fn convert(src: &str, script: &str) -> Option<QuestDef> {
    let s = read_script(src)?;
    let mut c = Conv { s: &s, review: Vec::new() };
    let mut talks = Vec::new();
    if let Some(stmts) = body(&s, "onTalk") {
        c.talks(&stmts, None, vec![], &mut talks);
    }
    for hook in s.methods.keys().filter(|m| m.starts_with("on") && *m != "onTalk") {
        c.review.push(format!("hook {hook} not run by the engine yet"));
    }
    let mut scenes: BTreeMap<u16, Vec<Rule>> = BTreeMap::new();
    let mut names: Vec<&String> = s.methods.keys().collect();
    names.sort();
    for name in names {
        let Some(n) = name.strip_prefix("Scene") else { continue };
        let Some(num) = n.strip_suffix("Return").and_then(|n| n.parse::<u16>().ok()) else { continue };
        let stmts = body(&s, name).unwrap_or_default();
        let rules: Vec<Rule> = c
            .paths(&stmts, 0)
            .into_iter()
            .filter(|(_, a)| !a.is_empty())
            .map(|(when, then)| Rule { when, then })
            .collect();
        if !rules.is_empty() {
            scenes.insert(num, rules);
        }
    }
    let mut review = c.review;
    review.sort();
    review.dedup();
    Some(QuestDef { quest: s.quest, script: script.to_owned(), source: Source::Sapphire, talks, scenes, review })
}

/// Every script under `dir` whose quest the β has, written as `<out>/<script>.json`; the quests
/// written.
pub fn run(dir: &Path, out: &Path, beta_scripts: &HashMap<u32, String>) -> anyhow::Result<Vec<u32>> {
    std::fs::create_dir_all(out)?;
    let mut written = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let p = entry?.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("cpp") {
                continue;
            }
            let src = std::fs::read_to_string(&p)?;
            let Some(quest) = read_script(&src).map(|s| s.quest) else { continue };
            let Some(script) = beta_scripts.get(&quest) else { continue };
            if let Some(def) = convert(&src, script) {
                std::fs::write(out.join(format!("{script}.json")), serde_json::to_string_pretty(&def)? + "\n")?;
                written.push(quest);
            }
        }
    }
    written.sort();
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAN_FST_001: &str = r#"
class ManFst001 : public Sapphire::ScriptAPI::QuestScript
{
private:
  static constexpr auto SEQ_0 = 0;
  static constexpr auto SEQ_FINISH = 255;
  static constexpr auto ACTOR0 = 1001148;
  static constexpr auto ACTOR1 = 1001140;
  static constexpr auto OPENING_EVENT_HANDLER = 1245186;
  void Scene00000( World::Quest& quest, Entity::Player& player )
  {
    eventMgr().playQuestScene( player, getId(), 0, HIDE_HOTBAR, bindSceneReturn( &ManFst001::Scene00000Return ) );
  }
  void Scene00000Return( World::Quest& quest, Entity::Player& player, const Event::SceneResult& result )
  {
    if( result.getResult( 0 ) == 1 ) // accept quest
    {
      player.setOpeningSequence( 2 );
      Scene00001( quest, player );
    }
  }
  void Scene00002Return( World::Quest& quest, Entity::Player& player, const Event::SceneResult& result )
  {
    quest.setSeq( SEQ_FINISH );
    eventMgr().eventStart( player, result.actorId, OPENING_EVENT_HANDLER, Event::EventHandler::Nest, 0, 0 );
    eventMgr().playQuestScene( player, OPENING_EVENT_HANDLER, 0x1E, HIDE_HOTBAR | NO_DEFAULT_CAMERA );
  }
  void Scene00005Return( World::Quest& quest, Entity::Player& player, const Event::SceneResult& result )
  {
    if( result.getResult( 0 ) == 1 )
    {
      player.finishQuest( getId(), result.getResult( 1 ) );
    }
  }
public:
  ManFst001() : Sapphire::ScriptAPI::QuestScript( 65575 ) {}
  void onTalk( World::Quest& quest, Entity::Player& player, uint64_t actorId ) override
  {
    if( actorId == ACTOR0 )
      Scene00000( quest, player );
    else if( actorId == ACTOR1 )
      Scene00004( quest, player );
  }
};
"#;

    #[test]
    fn coming_to_gridania() {
        let d = convert(MAN_FST_001, "ManFst001_00039").unwrap();
        assert_eq!(d.quest, 65575);
        assert_eq!(
            d.talks,
            [
                Talk { actor: 1001148, when: vec![], scene: 0, action: None },
                Talk { actor: 1001140, when: vec![], scene: 4, action: None }
            ]
        );
        let r1 = Cond::Result { index: 0, op: Op::Eq, value: 1 };
        assert_eq!(d.scenes[&0], [Rule { when: vec![r1.clone()], then: vec![Action::OpeningSeq { seq: 2 }, Action::Play { scene: 1 }] }]);
        assert_eq!(
            d.scenes[&2],
            [Rule { when: vec![], then: vec![Action::SetSeq { seq: 255 }, Action::OpeningScene { scene: 30 }] }]
        );
        assert_eq!(d.scenes[&5], [Rule { when: vec![r1], then: vec![Action::Complete { choice: Some(1) }] }]);
        assert!(d.review.is_empty(), "{:?}", d.review);
    }

    #[test]
    fn talk_switch_with_sequences_and_variables() {
        let src = r#"
enum Sequence : uint8_t { Seq0 = 0, Seq1 = 1, SeqFinish = 255, };
static constexpr auto Actor1 = 1002732;
void checkQuestCompletion( World::Quest& quest, Entity::Player& player )
{
  quest.setUI8AL( 0 );
  quest.setSeq( SeqFinish );
}
void onTalk( World::Quest& quest, Entity::Player& player, uint64_t actorId ) override
{
  switch( actorId )
  {
    case Actor1:
    {
      if( quest.getSeq() == Seq1 )
      {
        if( quest.getUI8AL() != 1 )
          Scene00005( quest, player );
      }
      else if( quest.getSeq() == SeqFinish )
        Scene00013( quest, player );
      break;
    }
  }
}
void Scene00005Return( World::Quest& quest, Entity::Player& player, const Event::SceneResult& result )
{
  if ( result.getResult( 0 ) == 1 )
  {
    quest.setUI8AL( 1 );
    quest.setBitFlag8( 1, true );
    checkQuestCompletion( quest, player );
    warpMgr().requestMoveTerritory( player, Common::WarpType::WARP_TYPE_NORMAL, teriMgr().getTerritoryByTypeId( 181 )->getGuId(), { 9.0f, 40.0f, 14.0f }, 0.0f );
  }
}
QuestScript( 65643 )
"#;
        let d = convert(src, "ManSea001_00107").unwrap();
        let seq = |v| Cond::Seq { op: Op::Eq, value: v };
        let al = Var::parse("UI8AL").unwrap();
        assert_eq!(
            d.talks,
            [
                Talk { actor: 1002732, when: vec![seq(1), Cond::Var { var: al, op: Op::Ne, value: 1 }], scene: 5, action: None },
                Talk { actor: 1002732, when: vec![Cond::Seq { op: Op::Ne, value: 1 }, seq(255)], scene: 13, action: None },
            ]
        );
        assert_eq!(
            d.scenes[&5][0].then,
            [
                Action::SetVar { var: al, value: 1 },
                Action::SetFlag { index: 1, set: true },
                Action::SetVar { var: al, value: 0 },
                Action::SetSeq { seq: 255 },
                Action::Warp { territory: 181, pos: [9.0, 40.0, 14.0], rotation: 0.0 },
            ]
        );
    }
}
