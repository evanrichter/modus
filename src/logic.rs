// Copyright 2021 Sergey Mechtaev

// This file is part of Modus.

// Modus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Modus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Modus.  If not, see <https://www.gnu.org/licenses/>.

//! This module contains logical structures that define the intermediate language used by Modus.
//!
//! Currently, these structures are generic, parameterized over the types they may use for constants
//! or variables.

use nom_locate::LocatedSpan;

use crate::logic::parser::Span;
use crate::unification::Rename;
use crate::{modusfile, sld};

use std::convert::TryInto;
use std::fmt;
use std::fmt::{Debug, Display};
use std::rc::Rc;
use std::str;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{collections::HashSet, hash::Hash};

impl fmt::Display for IRTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IRTerm::Constant(s) => write!(f, "\"{}\"", s),
            IRTerm::UserVariable(s) => write!(f, "{}", s),
            // there may be aux variables after translating to IR
            IRTerm::AuxiliaryVariable(i) => write!(f, "__AUX_{}", i),
            _ => unimplemented!(),
        }
    }
}

pub static AVAILABLE_VARIABLE_INDEX: AtomicU32 = AtomicU32::new(0);

impl Rename<IRTerm> for IRTerm {
    fn rename(&self) -> IRTerm {
        match self {
            IRTerm::Constant(_) => (*self).clone(),
            _ => {
                let index = AVAILABLE_VARIABLE_INDEX.fetch_add(1, Ordering::SeqCst);
                IRTerm::RenamedVariable(index, Box::new((*self).clone()))
            }
        }
    }
}

impl sld::Auxiliary for IRTerm {
    fn aux() -> IRTerm {
        let index = AVAILABLE_VARIABLE_INDEX.fetch_add(1, Ordering::SeqCst);
        IRTerm::AuxiliaryVariable(index)
    }
}

/// A predicate symbol
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Predicate(pub String);

impl From<String> for Predicate {
    fn from(s: String) -> Self {
        Predicate(s)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum IRTerm {
    Constant(String),
    UserVariable(String),
    AuxiliaryVariable(u32),
    RenamedVariable(u32, Box<IRTerm>),
}

impl IRTerm {
    pub fn as_constant(&self) -> Option<&str> {
        match self {
            IRTerm::Constant(c) => Some(&c[..]),
            _ => None,
        }
    }
}

/// Structure that holds information about the position of some section of the source code.
///
/// Not to be confused with `parser::Span`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SpannedPosition {
    pub line: u32,

    /// Index of the column. Assumes ASCII text (i.e. each character is a byte).
    pub column: usize,

    /// The relative offset of this spanned position from the original input.
    offset: usize,
    // TODO: length of the span
}

impl From<Span<'_>> for SpannedPosition {
    fn from(s: Span) -> Self {
        SpannedPosition {
            line: s.location_line(),
            column: s.get_column(),
            offset: s.location_offset(),
        }
    }
}

impl Default for SpannedPosition {
    fn default() -> Self {
        Self {
            line: 1,
            column: 1,
            offset: 0,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Literal<T = IRTerm> {
    pub position: Option<SpannedPosition>,
    pub predicate: Predicate,
    pub args: Vec<T>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Signature(pub Predicate, pub u32);

#[derive(Clone, PartialEq, Debug)]
pub struct Clause<T = IRTerm> {
    pub head: Literal<T>,
    pub body: Vec<Literal<T>>,
}

pub trait Ground {
    fn is_ground(&self) -> bool;
}

impl IRTerm {
    pub fn variables(&self) -> HashSet<IRTerm> {
        // the 'variables' of an IRTerm is just itself, if it's not a constant
        let mut set = HashSet::<IRTerm>::new();
        if let IRTerm::Constant(_) = self {
        } else {
            set.insert(self.clone());
        }
        set
    }
}

impl Literal {
    pub fn signature(&self) -> Signature {
        Signature(self.predicate.clone(), self.args.len().try_into().unwrap())
    }
    pub fn variables(&self) -> HashSet<IRTerm> {
        self.args
            .iter()
            .map(|r| r.variables())
            .reduce(|mut l, r| {
                l.extend(r);
                l
            })
            .unwrap_or_default()
    }
}

impl Clause {
    pub fn variables(&self) -> HashSet<IRTerm> {
        let mut body = self
            .body
            .iter()
            .map(|r| r.variables())
            .reduce(|mut l, r| {
                l.extend(r);
                l
            })
            .unwrap_or_default();
        body.extend(self.head.variables());
        body
    }
}

impl Ground for IRTerm {
    fn is_ground(&self) -> bool {
        return if let IRTerm::Constant(_) = self {
            true
        } else {
            false
        };
    }
}

impl Ground for Literal {
    fn is_ground(&self) -> bool {
        self.variables().is_empty()
    }
}

impl Ground for Clause {
    fn is_ground(&self) -> bool {
        self.variables().is_empty()
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.0, self.1)
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn display_sep<T: fmt::Display>(seq: &[T], sep: &str) -> String {
    return seq
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<String>>()
        .join(sep);
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.args {
            [] => write!(f, "{}", self.predicate),
            _ => write!(f, "{}({})", self.predicate, display_sep(&self.args, ", ")),
        }
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} :- {}", self.head, display_sep(&self.body, ", "))
    }
}

impl str::FromStr for Clause {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s.into());
        match parser::clause(parser::term)(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

impl str::FromStr for Literal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s.into());
        match parser::literal(parser::term)(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

/// The parser for the IR is only for convenience in writing tests.
pub mod parser {

    use super::*;

    use nom::{
        branch::alt,
        bytes::complete::{is_not, tag},
        character::complete::{alpha1, alphanumeric1, space0},
        combinator::{map, opt, recognize},
        error::VerboseError,
        multi::{many0, separated_list0, separated_list1},
        sequence::{delimited, pair, preceded, separated_pair, terminated},
    };
    use nom_locate::position;

    pub type Span<'a> = LocatedSpan<&'a str>;

    /// Redeclaration that uses VerboseError instead of the default nom::Error.
    pub type IResult<T, O> = nom::IResult<T, O, VerboseError<T>>;

    fn ws<'a, F: 'a, O>(inner: F) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, O>
    where
        F: FnMut(Span<'a>) -> IResult<Span<'a>, O>,
    {
        delimited(space0, inner, space0)
    }

    fn constant(i: Span) -> IResult<Span, Span> {
        delimited(tag("\""), is_not("\""), tag("\""))(i)
    }

    fn variable(i: Span) -> IResult<Span, Span> {
        literal_identifier(i)
    }

    pub fn term(i: Span) -> IResult<Span, IRTerm> {
        alt((
            map(constant, |s| IRTerm::Constant(s.fragment().to_string())),
            map(variable, |s| IRTerm::UserVariable(s.fragment().to_string())),
        ))(i)
    }

    //TODO: I need to think more carefully how to connect this to stage name
    pub fn literal_identifier(i: Span) -> IResult<Span, Span> {
        recognize(pair(
            alpha1,
            many0(alt((alphanumeric1, tag("_"), tag("-")))),
        ))(i)
    }

    /// Parses a literal with a generic term type.
    pub fn literal<'a, FT: 'a, T>(term: FT) -> impl FnMut(Span) -> IResult<Span, Literal<T>>
    where
        FT: FnMut(Span) -> IResult<Span, T> + Clone,
    {
        move |i| {
            let (i, pos) = position(i)?;

            let x = map(
                pair(
                    literal_identifier,
                    opt(delimited(
                        terminated(tag("("), space0),
                        separated_list1(ws(tag(",")), term.clone()),
                        preceded(space0, tag(")")),
                    )),
                ),
                |(name, args)| match args {
                    Some(args) => Literal {
                        position: Some(pos.into()),
                        predicate: Predicate(name.fragment().to_string()),
                        args,
                    },
                    None => Literal {
                        position: Some(pos.into()),
                        predicate: Predicate(name.fragment().to_string()),
                        args: Vec::new(),
                    },
                },
            )(i);
            x
        }
    }

    pub fn clause<'a, FT: 'a, T>(term: FT) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, Clause<T>>
    where
        FT: FnMut(Span) -> IResult<Span, T> + Clone,
    {
        map(
            separated_pair(
                literal(term.clone()),
                ws(tag(":-")),
                separated_list0(ws(tag(",")), literal(term)),
            ),
            |(head, body)| Clause { head, body },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        let l1 = Literal {
            position: Some(Default::default()),
            predicate: Predicate("l1".into()),
            args: vec![IRTerm::Constant("c".into())],
        };

        assert_eq!("l1(\"c\")", l1.to_string());
        assert_eq!(Ok(l1), "l1(\"c\")".parse());
    }

    #[test]
    fn simple_rule() {
        let c = IRTerm::Constant("c".into());
        let va = IRTerm::UserVariable("A".into());
        let vb = IRTerm::UserVariable("B".into());
        let l1 = Literal {
            position: None,
            predicate: Predicate("l1".into()),
            args: vec![va.clone(), vb.clone()],
        };
        let l2 = Literal {
            position: None,
            predicate: Predicate("l2".into()),
            args: vec![va.clone(), c.clone()],
        };
        let l3 = Literal {
            position: None,
            predicate: Predicate("l3".into()),
            args: vec![vb.clone(), c.clone()],
        };
        let r = Clause {
            head: l1,
            body: vec![l2, l3],
        };
        assert_eq!("l1(A, B) :- l2(A, \"c\"), l3(B, \"c\")", r.to_string());
        assert_eq!(Ok(r), "l1(A, B) :- l2(A, \"c\"), l3(B, \"c\")".parse());
    }

    #[test]
    fn nullary_predicate() {
        let va = IRTerm::UserVariable("A".into());
        let l1 = Literal {
            position: None,
            predicate: Predicate("l1".into()),
            args: Vec::new(),
        };
        let l2 = Literal {
            position: None,
            predicate: Predicate("l2".into()),
            args: vec![va.clone()],
        };
        let r = Clause {
            head: l1,
            body: vec![l2],
        };
        assert_eq!("l1 :- l2(A)", r.to_string());
        assert_eq!(Ok(r), "l1 :- l2(A)".parse());
    }
}
