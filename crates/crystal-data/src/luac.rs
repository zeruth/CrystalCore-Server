//! The client's content scripts (`game_script/**/*.luab`): compiled Lua 5.1 (header `1b 4c 75 61
//! 51 00 01 04 04 04 08 00`: little endian, 4-byte int, size_t and instruction, 8-byte number).
//! Read for what the server needs to know about them, not run: each function a script assigns
//! to its class (`ManFst001.OnScene00000 = function ...`), the methods it calls (`A0:QuestOffer`,
//! `A1:Talk`, ...) and the numbers it returns.

use std::collections::BTreeMap;

use crate::{DataError, Result};

fn bad(what: &str) -> DataError {
    DataError::Format { path: "game_script".into(), what: what.into() }
}

/// What a script's named functions do, by name (`OnScene00000`, `IsAcceptEvent`, ...).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    pub functions: BTreeMap<String, Function>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Function {
    /// The methods and fields it reads by name, in order of first use (`QuestOffer`, `Talk`,
    /// `TEXT_..`): a `SELF` or `GETTABLE` with a constant key.
    pub uses: Vec<String>,
    /// The numbers it can return (a `RETURN` of a register last loaded with a number constant).
    pub returns: Vec<f64>,
}

impl Script {
    /// The scene functions, by scene number (`OnScene00004` â†’ 4).
    pub fn scenes(&self) -> BTreeMap<u16, &Function> {
        self.functions
            .iter()
            .filter_map(|(name, f)| Some((name.strip_prefix("OnScene")?.parse().ok()?, f)))
            .collect()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        const HEADER: [u8; 12] = [0x1b, b'L', b'u', b'a', 0x51, 0, 1, 4, 4, 4, 8, 0];
        if bytes.get(..12) != Some(&HEADER) {
            return Err(bad("not Lua 5.1 bytecode as the client compiles it"));
        }
        let mut r = Reader { b: bytes, at: 12 };
        let main = r.function()?;
        let mut script = Script::default();
        collect(&main, &mut script);
        Ok(script)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Constant {
    Nil,
    Bool(bool),
    Number(f64),
    Str(String),
}

struct Proto {
    code: Vec<u32>,
    constants: Vec<Constant>,
    protos: Vec<Proto>,
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let s = self.b.get(self.at..self.at + n).ok_or_else(|| bad("Lua bytecode cut short"))?;
        self.at += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        // stored with its terminating NUL (a length of 0: no string)
        Ok(String::from_utf8_lossy(s.strip_suffix(&[0]).unwrap_or(s)).into_owned())
    }

    fn function(&mut self) -> Result<Proto> {
        self.string()?; // source
        self.u32()?; // line defined
        self.u32()?; // last line defined
        self.take(4)?; // upvalues, parameters, vararg flag, stack size
        let n = self.u32()? as usize;
        let code = (0..n).map(|_| self.u32()).collect::<Result<Vec<_>>>()?;
        let n = self.u32()? as usize;
        let mut constants = Vec::with_capacity(n);
        for _ in 0..n {
            constants.push(match self.u8()? {
                0 => Constant::Nil,
                1 => Constant::Bool(self.u8()? != 0),
                3 => Constant::Number(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
                4 => Constant::Str(self.string()?),
                t => return Err(bad(&format!("Lua constant type {t}"))),
            });
        }
        let n = self.u32()? as usize;
        let protos = (0..n).map(|_| self.function()).collect::<Result<Vec<_>>>()?;
        // debug information: line numbers, locals, upvalue names
        let n = self.u32()? as usize;
        self.take(n * 4)?;
        let n = self.u32()?;
        for _ in 0..n {
            self.string()?;
            self.take(8)?;
        }
        let n = self.u32()?;
        for _ in 0..n {
            self.string()?;
        }
        Ok(Proto { code, constants, protos })
    }
}

// Lua 5.1 opcodes (lopcodes.h)
const LOADK: u32 = 1;
const GETTABLE: u32 = 6;
const SETTABLE: u32 = 9;
const SELF: u32 = 11;
const RETURN: u32 = 30;
const CLOSURE: u32 = 36;

fn op(i: u32) -> u32 {
    i & 0x3f
}
fn a(i: u32) -> u32 {
    (i >> 6) & 0xff
}
fn c(i: u32) -> u32 {
    (i >> 14) & 0x1ff
}
fn b(i: u32) -> u32 {
    (i >> 23) & 0x1ff
}
fn bx(i: u32) -> u32 {
    i >> 14
}

/// An RK operand's constant string, when it names a constant (bit 8 set).
fn rk_str(p: &Proto, rk: u32) -> Option<&str> {
    if rk & 0x100 == 0 {
        return None;
    }
    match p.constants.get((rk & 0xff) as usize)? {
        Constant::Str(s) => Some(s),
        _ => None,
    }
}

/// Every function a proto assigns to a table under a string key (`T.Name = function`), named so,
/// and recursively the functions inside those.
fn collect(p: &Proto, out: &mut Script) {
    let mut closure_in: BTreeMap<u32, usize> = BTreeMap::new();
    for &i in &p.code {
        match op(i) {
            CLOSURE => {
                closure_in.insert(a(i), bx(i) as usize);
            }
            SETTABLE => {
                if let (Some(name), Some(&proto)) = (rk_str(p, b(i)), closure_in.get(&c(i)))
                    && let Some(f) = p.protos.get(proto)
                {
                    out.functions.insert(name.to_owned(), facts(f));
                }
            }
            _ => {}
        }
    }
    for child in &p.protos {
        collect(child, out);
    }
}

fn facts(p: &Proto) -> Function {
    let mut f = Function::default();
    let mut number_in: BTreeMap<u32, f64> = BTreeMap::new();
    for &i in &p.code {
        match op(i) {
            SELF | GETTABLE => {
                if let Some(name) = rk_str(p, c(i))
                    && !f.uses.iter().any(|u| u == name)
                {
                    f.uses.push(name.to_owned());
                }
                number_in.remove(&a(i));
            }
            LOADK => match p.constants.get(bx(i) as usize) {
                Some(Constant::Number(n)) => {
                    number_in.insert(a(i), *n);
                }
                _ => {
                    number_in.remove(&a(i));
                }
            },
            RETURN if b(i) >= 2 => {
                if let Some(&n) = number_in.get(&a(i))
                    && !f.returns.contains(&n)
                {
                    f.returns.push(n);
                }
            }
            _ => {
                number_in.remove(&a(i));
            }
        }
    }
    f
}
