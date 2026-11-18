use std::cmp::min;
use std::collections::HashMap;
use std::fmt;
use std::mem;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use serde::de::{Deserialize, Deserializer, Error, Visitor};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeError {
    #[error("Tried to restore cleared scopes, but none were cleared")]
    NoClearedScopesToRestore,
}

static SCOPE_REPO: LazyLock<Mutex<ScopeRepository>> =
    LazyLock::new(|| Mutex::new(ScopeRepository::new()));

fn lock_global_scope_repo() -> MutexGuard<'static, ScopeRepository> {
    SCOPE_REPO.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default, Hash)]
pub struct Scope {
    a: u64,
    b: u64,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParseScopeError {
    #[error("Too long scope. Scopes can be at most 8 atoms long.")]
    TooLong,
    #[error("Too many atoms. Max 2^16-2 atoms allowed.")]
    TooManyAtoms,
}

#[derive(Debug)]
struct ScopeRepository {
    atoms: Vec<String>,
    atom_index_map: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeStack {
    clear_stack: Vec<Vec<Scope>>,
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Deserialize)]
pub enum ClearAmount {
    TopN(usize),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeStackOp {
    Push(Scope),
    Pop(usize),
    Clear(ClearAmount),
    Restore,
    Noop,
}

fn pack_as_u16s(atoms: &[usize]) -> Result<Scope, ParseScopeError> {
    let mut res = Scope { a: 0, b: 0 };
    for (i, &n) in atoms.iter().enumerate() {
        if n >= (u16::MAX as usize) - 2 {
            return Err(ParseScopeError::TooManyAtoms);
        }
        let small = (n + 1) as u64;
        if i < 4 {
            res.a |= small << ((3 - i) * 16);
        } else if i < 8 {
            res.b |= small << ((7 - i) * 16);
        } else {
            return Err(ParseScopeError::TooLong);
        }
    }
    Ok(res)
}

impl ScopeRepository {
    fn new() -> ScopeRepository {
        ScopeRepository {
            atoms: Vec::new(),
            atom_index_map: HashMap::new(),
        }
    }

    fn build(&mut self, s: &str) -> Result<Scope, ParseScopeError> {
        if s.is_empty() {
            return Ok(Scope { a: 0, b: 0 });
        }
        let parts: Vec<usize> = s
            .trim_end_matches('.')
            .split('.')
            .map(|a| self.atom_to_index(a))
            .collect();
        if parts.len() > 8 {
            return Err(ParseScopeError::TooLong);
        }
        pack_as_u16s(&parts)
    }

    fn to_string(&self, scope: Scope) -> String {
        let mut s = String::new();
        for i in 0..8 {
            let atom_number = scope.atom_at(i);
            if atom_number == 0 {
                break;
            }
            if i != 0 {
                s.push('.');
            }
            s.push_str(self.atom_str(atom_number));
        }
        s
    }

    fn atom_to_index(&mut self, atom: &str) -> usize {
        if let Some(index) = self.atom_index_map.get(atom) {
            return *index;
        }
        self.atoms.push(atom.to_owned());
        let index = self.atoms.len() - 1;
        self.atom_index_map.insert(atom.to_owned(), index);
        index
    }

    fn atom_str(&self, atom_number: u16) -> &str {
        usize::from(atom_number)
            .checked_sub(1)
            .and_then(|i| self.atoms.get(i))
            .map_or("", String::as_str)
    }
}

impl Scope {
    pub fn new(s: &str) -> Result<Scope, ParseScopeError> {
        let mut repo = lock_global_scope_repo();
        repo.build(s.trim())
    }

    pub fn atom_at(self, index: usize) -> u16 {
        let shifted = if index < 4 {
            self.a >> ((3 - index) * 16)
        } else if index < 8 {
            self.b >> ((7 - index) * 16)
        } else {
            0
        };
        (shifted & 0xFFFF) as u16
    }

    #[inline]
    fn missing_atoms(self) -> u32 {
        let trail = if self.b == 0 {
            self.a.trailing_zeros() + 64
        } else {
            self.b.trailing_zeros()
        };
        trail / 16
    }

    #[inline(always)]
    pub fn len(self) -> u32 {
        8 - self.missing_atoms()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn build_string(self) -> String {
        let repo = lock_global_scope_repo();
        repo.to_string(self)
    }

    pub fn is_prefix_of(self, s: Scope) -> bool {
        let pref_missing = self.missing_atoms();
        let mask: (u64, u64) = if pref_missing == 8 {
            (0, 0)
        } else if pref_missing == 4 {
            (u64::MAX, 0)
        } else if pref_missing > 4 {
            (u64::MAX << ((pref_missing - 4) * 16), 0)
        } else {
            (u64::MAX, u64::MAX << (pref_missing * 16))
        };
        let ax = (self.a ^ s.a) & mask.0;
        let bx = (self.b ^ s.b) & mask.1;
        ax == 0 && bx == 0
    }
}

impl FromStr for Scope {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<Scope, ParseScopeError> {
        Scope::new(s)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.build_string())
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.build_string())
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ScopeVisitor;

        impl Visitor<'_> for ScopeVisitor {
            type Value = Scope;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E>(self, v: &str) -> Result<Scope, E>
            where
                E: Error,
            {
                Scope::new(v).map_err(|e| Error::custom(format!("Invalid scope: {e:?}")))
            }
        }

        deserializer.deserialize_str(ScopeVisitor)
    }
}

impl ScopeStack {
    pub fn new() -> ScopeStack {
        ScopeStack::default()
    }

    pub fn from_vec(v: Vec<Scope>) -> ScopeStack {
        ScopeStack {
            clear_stack: Vec::new(),
            scopes: v,
        }
    }

    #[inline]
    pub fn push(&mut self, s: Scope) {
        self.scopes.push(s);
    }

    #[inline]
    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    pub fn apply(&mut self, op: &ScopeStackOp) -> Result<(), ScopeError> {
        match *op {
            ScopeStackOp::Push(scope) => self.scopes.push(scope),
            ScopeStackOp::Pop(count) => {
                let keep = self.scopes.len().saturating_sub(count);
                self.scopes.truncate(keep);
            }
            ScopeStackOp::Clear(amount) => {
                let cleared = match amount {
                    ClearAmount::TopN(n) => {
                        let to_leave = self.scopes.len() - min(n, self.scopes.len());
                        self.scopes.split_off(to_leave)
                    }
                    ClearAmount::All => mem::take(&mut self.scopes),
                };
                self.clear_stack.push(cleared);
            }
            ScopeStackOp::Restore => match self.clear_stack.pop() {
                Some(to_push) => self.scopes.extend(to_push),
                None => return Err(ScopeError::NoClearedScopesToRestore),
            },
            ScopeStackOp::Noop => (),
        }
        Ok(())
    }

    #[inline]
    pub fn as_slice(&self) -> &[Scope] {
        &self.scopes
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FromStr for ScopeStack {
    type Err = ParseScopeError;

    fn from_str(s: &str) -> Result<ScopeStack, ParseScopeError> {
        let scopes = s
            .split_whitespace()
            .map(Scope::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ScopeStack::from_vec(scopes))
    }
}

impl fmt::Display for ScopeStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.scopes {
            write!(f, "{s} ")?;
        }
        Ok(())
    }
}
