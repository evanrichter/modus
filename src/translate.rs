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

use nom::{bytes::streaming::tag, sequence::delimited};

use crate::{
    logic::{self, IRTerm},
    modusfile::{
        parser::{modus_var, outside_format_expansion},
        Expression, ModusClause, ModusTerm,
    },
    sld::Auxiliary,
};

/// Takes the content of a format string.
/// Returns an IRTerm to be used instead of the format string term, and a list of literals
/// needed to make this equivalent.
fn convert_format_string(format_string_content: &str) -> (Vec<logic::Literal>, IRTerm) {
    let concat_predicate = "string_concat";
    let mut curr_string = format_string_content;
    let mut prev_variable: IRTerm = Auxiliary::aux();
    let mut new_literals = vec![logic::Literal {
        // this initial literal is a no-op that makes the code simpler
        predicate: logic::Predicate(concat_predicate.to_owned()),
        args: vec![
            IRTerm::Constant("".to_owned()),
            IRTerm::Constant("".to_owned()),
            prev_variable.clone(),
        ],
    }];

    // Approach is to parse sections of the string and create new literals, e.g.
    // if the last var we created was R1 and we just parsed some (constant) string c, we
    // add a literal `string_concat(R1, c, R2)`, creating a new variable R2.
    while !curr_string.is_empty() {
        let (i, constant_str) =
            outside_format_expansion(curr_string).expect("can parse outside format expansion");
        let constant_str = constant_str.replace("\\$", "$");
        let new_var: IRTerm = Auxiliary::aux();
        let new_literal = logic::Literal {
            predicate: logic::Predicate(concat_predicate.to_string()),
            args: vec![
                prev_variable,
                IRTerm::Constant(constant_str),
                new_var.clone(),
            ],
        };
        new_literals.push(new_literal);
        prev_variable = new_var;

        // this might fail, e.g. if we are at the end of the string
        let variable_res = delimited(tag("${"), modus_var, tag("}"))(i);
        if let Ok((rest, variable)) = variable_res {
            let new_var: IRTerm = Auxiliary::aux();
            let new_literal = logic::Literal {
                predicate: logic::Predicate(concat_predicate.to_string()),
                args: vec![
                    prev_variable,
                    IRTerm::UserVariable(variable.to_owned()),
                    new_var.clone(),
                ],
            };
            new_literals.push(new_literal);
            prev_variable = new_var;
            curr_string = rest;
        } else {
            curr_string = "";
        }
    }
    (new_literals, prev_variable)
}

impl From<&crate::modusfile::ModusClause> for Vec<logic::Clause> {
    /// Convert a ModusClause into one supported by the IR.
    /// It converts logical or/; into multiple rules, which should be equivalent.
    fn from(modus_clause: &crate::modusfile::ModusClause) -> Self {
        let mut clauses: Vec<logic::Clause> = Vec::new();

        // REVIEW: lots of cloning going on below, double check if this is necessary.
        match &modus_clause.body {
            Some(Expression::Literal(l)) => {
                let mut literals: Vec<logic::Literal> = Vec::new();
                let mut new_literal_args: Vec<logic::IRTerm> = Vec::new();

                for arg in &l.args {
                    new_literal_args.push(match arg {
                        ModusTerm::Constant(c) => IRTerm::Constant(c.to_owned()),
                        ModusTerm::FormatString(s) => {
                            let (new_literals, new_var) = convert_format_string(s);
                            literals.extend(new_literals);
                            new_var
                        }
                        ModusTerm::UserVariable(v) => IRTerm::UserVariable(v.to_owned()),
                    })
                }
                literals.push(logic::Literal {
                    predicate: l.predicate.clone(),
                    args: new_literal_args,
                });
                clauses.push(logic::Clause {
                    head: modus_clause.head.clone().into(),
                    body: literals,
                });
            }
            // ignores operators for now
            Some(Expression::OperatorApplication(expr, _)) => {
                clauses.extend(Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr.clone()),
                }))
            }
            Some(Expression::And(expr1, expr2)) => {
                let c1 = Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr1.clone()),
                });
                let c2 = Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr2.clone()),
                });

                // If we have the possible rules for left and right sub expressions,
                // consider the cartesian product of them.
                for clause1 in &c1 {
                    for clause2 in &c2 {
                        clauses.push(logic::Clause {
                            head: clause1.head.clone(),
                            body: clause1
                                .body
                                .clone()
                                .into_iter()
                                .chain(clause2.body.clone().into_iter())
                                .collect(),
                        })
                    }
                }
            }
            Some(Expression::Or(expr1, expr2)) => {
                let c1 = Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr1.clone()),
                });
                let c2 = Self::from(&ModusClause {
                    head: modus_clause.head.clone(),
                    body: Some(*expr2.clone()),
                });

                clauses.extend(c1);
                clauses.extend(c2);
            }
            None => clauses.push(logic::Clause {
                head: modus_clause.head.clone().into(),
                body: Vec::new(),
            }),
        }
        clauses
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    /// Should be called if any tests rely on the variable index.
    /// Note that the code (currently) doesn't rely on the variable indexes, just the tests, for convenience.
    fn setup() {
        logic::AVAILABLE_VARIABLE_INDEX.store(0, std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    #[serial]
    fn format_string_translation() {
        setup();

        let case = "ubuntu:${distr_version}";

        let lits = vec![
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::Constant("".to_owned()), IRTerm::Constant("".to_owned()), IRTerm::AuxiliaryVariable(0)],
            },
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::AuxiliaryVariable(0), IRTerm::Constant("ubuntu:".to_owned()), IRTerm::AuxiliaryVariable(1)],
            },
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::AuxiliaryVariable(1), IRTerm::UserVariable("distr_version".to_owned()), IRTerm::AuxiliaryVariable(2)],
            },
        ];

        assert_eq!((lits, IRTerm::AuxiliaryVariable(2)), convert_format_string(case));
    }

    #[test]
    #[serial]
    fn format_string_translation_with_escape() {
        setup();

        let case = "use ${feature} like this \\${...}";

        let lits = vec![
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::Constant("".to_owned()), IRTerm::Constant("".to_owned()), IRTerm::AuxiliaryVariable(0)],
            },
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::AuxiliaryVariable(0), IRTerm::Constant("use ".to_owned()), IRTerm::AuxiliaryVariable(1)],
            },
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::AuxiliaryVariable(1), IRTerm::UserVariable("feature".to_owned()), IRTerm::AuxiliaryVariable(2)],
            },
            logic::Literal {
                predicate: logic::Predicate("string_concat".to_owned()),
                args: vec![IRTerm::AuxiliaryVariable(2), IRTerm::Constant(" like this ${...}".to_owned()), IRTerm::AuxiliaryVariable(3)],
            },
        ];

        assert_eq!((lits, IRTerm::AuxiliaryVariable(3)), convert_format_string(case));
    }
}
