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

use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    hash::Hash,
};

use crate::{
    builtin,
    unification::{compose_extend, compose_no_extend, Rename, Substitution},
};
use crate::{builtin::SelectBuiltinResult, unification::RenameWithSubstitution};
use crate::{
    logic::{self, Signature},
    unification::Substitute,
    wellformed,
};
use codespan_reporting::diagnostic::{Diagnostic, Label};
use logic::{Clause, IRTerm, Literal};

pub trait Auxiliary: Rename<Self> + Sized {
    fn aux() -> Self;
}

type RuleId = usize;
type TreeLevel = usize;
pub(crate) type Goal<T = IRTerm> = Vec<Literal<T>>;

/// A clause is either a rule, or a query
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ClauseId {
    Rule(RuleId),
    Query,
    Builtin(Literal<IRTerm>),
}

/// A literal origin can be uniquely identified through its source clause and its index in the clause body
#[derive(Clone, PartialEq, Debug)]
pub struct LiteralOrigin {
    clause: ClauseId,
    body_index: usize,
}

/// Literal identifier relative to the goal
type LiteralGoalId = usize;

/// literal, tree level at which it was introduced if any, where it came from
#[derive(Clone, PartialEq, Debug)]
struct LiteralWithHistory {
    literal: Literal<IRTerm>,
    introduction: TreeLevel,
    origin: LiteralOrigin,
}
type GoalWithHistory = Vec<LiteralWithHistory>;

/// An SLD tree consists of
/// - a goal with its dependencies (at which level and from which part of body each literal was introduced)
/// - a level, which is incremented as tree grows
/// - a mapping from (selected literal in goal, applied rule) to (mgu after rule renaming, rule renaming, resolvent subtree)
#[derive(Clone)]
pub struct Tree {
    goal: GoalWithHistory,
    level: TreeLevel,
    resolvents: HashMap<(LiteralGoalId, ClauseId), (Substitution, Substitution, Tree)>,
}

/// A proof tree consist of
/// - a clause
/// - a valuation for this clause
/// - proofs for parts of the clause body
#[derive(Clone, Debug)]
pub struct Proof {
    pub clause: ClauseId,
    pub valuation: Substitution,
    pub children: Vec<Proof>,
}

impl Substitute<IRTerm> for GoalWithHistory {
    type Output = GoalWithHistory;
    fn substitute(&self, s: &Substitution<IRTerm>) -> Self::Output {
        self.iter()
            .map(
                |LiteralWithHistory {
                     literal,
                     introduction,
                     origin,
                 }| LiteralWithHistory {
                    literal: literal.substitute(s),
                    introduction: introduction.clone(),
                    origin: origin.clone(),
                },
            )
            .collect()
    }
}

pub fn sld(
    rules: &Vec<Clause<IRTerm>>,
    goal: &Goal,
    maxdepth: TreeLevel,
) -> Result<Option<Tree>, Diagnostic<()>> {
    /// select leftmost literal with compatible groundness
    fn select(
        goal: &GoalWithHistory,
        grounded: &HashMap<Signature, Vec<bool>>,
    ) -> Result<Option<(LiteralGoalId, LiteralWithHistory)>, Diagnostic<()>> {
        for (id, lit) in goal.iter().enumerate() {
            let literal = &lit.literal;
            let select_builtin_res = builtin::select_builtin(literal);
            if select_builtin_res.0.is_match() {
                return Ok(Some((id, lit.clone())));
            }

            // For any user-defined atom, we can get its groundness requirement
            // (computed outside), and if a particular argument can not be
            // ungrounded (grounded[arg_index] == false), variables will not be
            // allowed there.
            let lit_grounded = grounded.get(&literal.signature());
            if let Some(lit_grounded) = lit_grounded {
                debug_assert_eq!(lit_grounded.len(), literal.args.len());
                if literal
                    .args
                    .iter()
                    .zip(lit_grounded.iter())
                    .all(|pair| matches!(pair, (_, true) | (IRTerm::Constant(_), false)))
                {
                    return Ok(Some((id, lit.clone())));
                } else {
                    continue;
                }
            } else if select_builtin_res.0 == SelectBuiltinResult::GroundnessMismatch {
                continue;
            }

            // No builtin nor user-define predicate with this name
            return Err(Diagnostic::error()
                .with_message("unknown predicate")
                .with_labels(vec![Label::primary(
                    (),
                    literal
                        .clone()
                        .position
                        .map(|span_pos| span_pos.offset..(span_pos.offset + span_pos.length))
                        .unwrap(),
                )]));
        }

        Ok(None)
    }

    fn resolve(
        lid: LiteralGoalId,
        rid: ClauseId,
        goal: &GoalWithHistory,
        mgu: &Substitution,
        rule: &Clause,
        level: TreeLevel,
    ) -> GoalWithHistory {
        let mut g: GoalWithHistory = goal.clone();
        g.remove(lid);
        g.extend(
            rule.body
                .iter()
                .enumerate()
                .map(|(id, l)| {
                    let origin = LiteralOrigin {
                        clause: rid.clone(),
                        body_index: id,
                    };
                    LiteralWithHistory {
                        literal: l.clone(),
                        introduction: level,
                        origin,
                    }
                })
                .collect::<GoalWithHistory>(),
        );
        g.substitute(mgu)
    }

    fn inner(
        rules: &Vec<Clause<IRTerm>>,
        goal: &GoalWithHistory,
        maxdepth: TreeLevel,
        level: TreeLevel,
        grounded: &HashMap<Signature, Vec<bool>>,
    ) -> Result<Option<Tree>, Diagnostic<()>> {
        #[cfg(debug_assertions)]
        {
            // FIXME: move this ad-hoc debug code elsewhere
            eprintln!(
                "{}inner(rules, goal=[ {} ], level={}/{})",
                "  ".to_string().repeat(level),
                goal.iter()
                    .map(|g| format!(
                        "{}({})",
                        &g.literal.predicate.0,
                        g.literal
                            .args
                            .iter()
                            .map(|x| match x {
                                IRTerm::Constant(x) => x.to_string(),
                                IRTerm::UserVariable(v) =>
                                    format!("{:?}", v).trim_matches('\"').to_string(),
                                _ => format!("{:?}", x),
                            })
                            .collect::<Vec<String>>()
                            .join(", ")
                    ))
                    .collect::<Vec<String>>()
                    .join(", "),
                level,
                maxdepth
            );
        }
        if goal.is_empty() {
            Ok(Some(Tree {
                goal: goal.clone(),
                level,
                resolvents: HashMap::new(),
            }))
        } else if level >= maxdepth {
            Ok(None)
        } else {
            let selected = select(goal, grounded)?;
            if selected.is_none() {
                return Ok(None);
            }
            let (lid, l) = selected.unwrap();

            let builtin_resolves = match builtin::select_builtin(&l.literal) {
                (SelectBuiltinResult::Match, lit) => lit,
                _ => None,
            }
            .and_then(|pred| pred.apply(&l.literal))
            .and_then(|unify_cand| {
                unify_cand.unify(&l.literal).map(|mgu| {
                    (
                        ClauseId::Builtin(unify_cand.clone()),
                        mgu.clone(),
                        Substitution::<IRTerm>::new(),
                        resolve(
                            lid,
                            ClauseId::Builtin(unify_cand.clone()),
                            &goal,
                            &mgu,
                            &Clause {
                                head: unify_cand,
                                body: Vec::new(), // TODO: allow builtin rules to return more conditions?
                            },
                            level + 1,
                        ),
                    )
                })
            })
            .into_iter();
            let user_rules_resolves = rules
                .iter()
                .enumerate()
                .filter(|(_, c)| c.head.signature() == l.literal.signature())
                .map(|(rid, c)| (ClauseId::Rule(rid), c.rename_with_sub()))
                .filter_map(|(rid, (c, renaming))| {
                    c.head.unify(&l.literal).map(|mgu| {
                        (
                            rid.clone(),
                            mgu.clone(),
                            renaming,
                            resolve(lid, rid, &goal, &mgu, &c, level + 1),
                        )
                    })
                });

            let mut resolvents: HashMap<
                (LiteralGoalId, ClauseId),
                (Substitution, Substitution, Tree),
            > = HashMap::new();
            for (rid, mgu, renaming, resolvent) in builtin_resolves.chain(user_rules_resolves) {
                let maybe_tree = inner(rules, &resolvent, maxdepth, level + 1, grounded)?;
                if let Some(tree) = maybe_tree {
                    resolvents.insert((lid, rid), (mgu, renaming, tree));
                }
            }
            if resolvents.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Tree {
                    goal: goal.clone(),
                    level,
                    resolvents,
                }))
            }
        }
    }

    let grounded = wellformed::check_grounded_variables(rules).unwrap();

    let goal_with_history = goal
        .iter()
        .enumerate()
        .map(|(id, l)| {
            let origin = LiteralOrigin {
                clause: ClauseId::Query,
                body_index: id,
            };
            LiteralWithHistory {
                literal: l.clone(),
                introduction: 0,
                origin,
            }
        })
        .collect();
    inner(rules, &goal_with_history, maxdepth, 0, &grounded)
}

pub fn solutions(tree: &Tree) -> HashSet<Goal> {
    fn inner(tree: &Tree) -> Vec<Substitution> {
        if tree.goal.is_empty() {
            let s = Substitution::new();
            return vec![s];
        }
        tree.resolvents
            .iter()
            .map(|(_, (mgu, _, subtree))| (mgu, inner(subtree)))
            .map(|(mgu, sub)| {
                sub.iter()
                    .map(|s| compose_extend(mgu, s))
                    .collect::<Vec<Substitution<IRTerm>>>()
            })
            .flatten()
            .collect()
    }
    inner(tree)
        .iter()
        .map(|s| {
            tree.goal
                .iter()
                .map(
                    |LiteralWithHistory {
                         literal,
                         introduction: _,
                         origin: _,
                     }| literal.substitute(s),
                )
                .collect()
        })
        .collect()
}

#[derive(Clone)]
struct PathNode {
    resolvent: GoalWithHistory,
    applied: ClauseId,
    selected: LiteralGoalId,
    renaming: Substitution,
}

// sequence of nodes and global mgu
type Path = (Vec<PathNode>, Substitution);

pub fn proofs(tree: &Tree, rules: &Vec<Clause>, goal: &Goal) -> Vec<Proof> {
    fn flatten_compose(
        lid: &LiteralGoalId,
        cid: &ClauseId,
        mgu: &Substitution,
        renaming: &Substitution,
        tree: &Tree,
    ) -> Vec<Path> {
        if tree.goal.is_empty() {
            return vec![(
                vec![PathNode {
                    resolvent: tree.goal.clone(),
                    applied: cid.clone(),
                    selected: lid.clone(),
                    renaming: renaming.clone(),
                }],
                mgu.clone(),
            )];
        }
        tree.resolvents
            .iter()
            .map(|((sub_lid, sub_cid), (sub_mgu, sub_renaming, sub_tree))| {
                flatten_compose(sub_lid, sub_cid, sub_mgu, sub_renaming, sub_tree)
                    .iter()
                    .map(|(sub_path, sub_val)| {
                        let mut nodes = vec![PathNode {
                            resolvent: tree.goal.clone(),
                            applied: cid.clone(),
                            selected: lid.clone(),
                            renaming: renaming.clone(),
                        }];
                        let val = compose_extend(mgu, sub_val);
                        nodes.extend(sub_path.clone());
                        (nodes, val)
                    })
                    .collect::<Vec<Path>>()
            })
            .flatten()
            .collect()
    }
    // reconstruct proof for a given tree level
    fn proof_for_level(
        path: &Vec<PathNode>,
        mgu: &Substitution,
        rules: &Vec<Clause>,
        level: TreeLevel,
    ) -> Proof {
        let mut sublevels_map: HashMap<usize, TreeLevel> = HashMap::new();
        for l in 0..path.len() {
            if !path[l].resolvent.is_empty() {
                let resolved_child = path[l].resolvent[path[l + 1].selected].clone();
                if resolved_child.introduction == level {
                    sublevels_map.insert(resolved_child.origin.body_index, l + 1);
                }
            }
        }
        let children_length = sublevels_map.len();
        match path[level].applied {
            ClauseId::Query => assert_eq!(children_length, path[0].resolvent.len()),
            ClauseId::Rule(rid) => assert_eq!(children_length, rules[rid].body.len()),
            ClauseId::Builtin(_) => assert_eq!(children_length, 0),
        };

        let mut sublevels = Vec::<TreeLevel>::with_capacity(sublevels_map.len());
        for k in sublevels_map.keys() {
            assert!(*k < children_length);
        }
        for i in 0..children_length {
            sublevels.push(*sublevels_map.get(&i).unwrap());
        }
        Proof {
            clause: path[level].applied.clone(),
            valuation: compose_no_extend(&path[level].renaming, mgu),
            children: sublevels
                .iter()
                .map(|l| proof_for_level(path, mgu, rules, *l))
                .collect(),
        }
    }
    // assume lid of root is 0, as if it came from a clause "true :- goal" for query "true", but this is not used anyway
    let goal_vars = goal
        .iter()
        .map(|l| l.variables())
        .reduce(|mut l, r| {
            l.extend(r);
            l
        })
        .unwrap_or_default();
    let goal_id_renaming: Substitution<IRTerm> =
        goal_vars.iter().map(|v| (v.clone(), v.clone())).collect();
    let paths = flatten_compose(
        &0,
        &ClauseId::Query,
        &Substitution::new(),
        &goal_id_renaming,
        tree,
    );
    let all_proofs: Vec<Proof> = paths
        .iter()
        .map(|(path, mgu)| proof_for_level(path, mgu, rules, 0))
        .collect();

    //TODO: instead, I should find optimal proofs
    let mut computed: HashSet<Goal> = HashSet::new();
    let mut proofs: Vec<Proof> = Vec::new();
    for p in all_proofs {
        let solution: Goal = goal.substitute(&p.valuation);
        if !computed.contains(&solution) {
            computed.insert(solution.clone());
            proofs.push(p)
        }
    }
    proofs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // These tests all need to be marked as serial even though they don't
    // interfere with each other, because they can potentially modify
    // logic::AVAILABLE_VARIABLE_INDEX, but tests in the translate module depend
    // on the variable not being changed by something else during execution.

    fn contains_ignoring_position(it: &HashSet<Vec<Literal>>, lits: &Vec<Literal>) -> bool {
        it.iter().any(|curr_lits| {
            curr_lits.len() == lits.len()
                && curr_lits
                    .iter()
                    .zip(lits)
                    .all(|(l1, l2)| l1.eq_ignoring_position(&l2))
        })
    }

    #[test]
    #[serial]
    fn simple_solving() {
        let goal: Goal<logic::IRTerm> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![
            "a(X) :- b(X).".parse().unwrap(),
            logic::Clause {
                head: "b(\"c\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "b(\"d\")".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 2);

        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"c\")".parse::<logic::Literal>().unwrap()]
        ));
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"d\")".parse::<logic::Literal>().unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn simple_nongrounded() {
        let goal: Goal<logic::IRTerm> = vec!["a(\"b\")".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![logic::Clause {
            head: "a(X)".parse().unwrap(),
            body: vec![],
        }];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 1);
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"b\")".parse::<logic::Literal>().unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn simple_nongrounded_invalid() {
        let goal: Goal<logic::IRTerm> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![logic::Clause {
            head: "a(X)".parse().unwrap(),
            body: vec![],
        }];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_none());
    }

    #[test]
    #[serial]
    fn complex_goal() {
        let goal: Goal<logic::IRTerm> = vec!["a(X)".parse().unwrap(), "b(X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![
            logic::Clause {
                head: "a(\"t\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "a(\"f\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "b(\"g\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "b(\"t\")".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 1);
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"t\")".parse().unwrap(), "b(\"t\")".parse().unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn solving_with_binary_relations() {
        let goal: Goal<logic::IRTerm> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![
            "a(X) :- b(X, Y), c(Y).".parse().unwrap(),
            logic::Clause {
                head: "b(\"t\", \"f\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "b(\"f\", \"t\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "b(\"g\", \"t\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "c(\"t\")".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 2);
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"f\")".parse().unwrap()]
        ));
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["a(\"g\")".parse().unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn simple_recursion() {
        let goal: Goal<logic::IRTerm> = vec!["reach(\"a\", X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![
            "reach(X, Y) :- reach(X, Z), arc(Z, Y).".parse().unwrap(),
            "reach(X, Y) :- arc(X, Y).".parse().unwrap(),
            logic::Clause {
                head: "arc(\"a\", \"b\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"b\", \"c\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"c\", \"d\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"d\", \"e\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"f\", \"e\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"g\", \"f\")".parse().unwrap(),
                body: vec![],
            },
            logic::Clause {
                head: "arc(\"g\", \"a\")".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 15).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 4);
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["reach(\"a\", \"b\")".parse().unwrap()]
        ));
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["reach(\"a\", \"c\")".parse().unwrap()]
        ));
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["reach(\"a\", \"d\")".parse().unwrap()]
        ));
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["reach(\"a\", \"e\")".parse().unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn string_concat() {
        let goal: Goal<logic::IRTerm> =
            vec!["string_concat(\"hello\", \"world\", X)".parse().unwrap()];
        let clauses: Vec<logic::Clause> = vec![];
        let result = sld(&clauses, &goal, 10).unwrap();
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 1);
        assert!(contains_ignoring_position(
            &solutions,
            &vec!["string_concat(\"hello\", \"world\", \"helloworld\")"
                .parse()
                .unwrap()]
        ));
    }

    #[test]
    #[serial]
    fn string_concat_complex() {
        // TODO: figure out how to test empty string
        let good = ["aaabbb", "aabb"];
        let bad = ["aab", "a", "bb"];
        for (s, is_good) in good
            .iter()
            .map(|x| (*x, true))
            .chain(bad.iter().map(|x| (*x, false)))
        {
            let goal: Goal<logic::IRTerm> = vec![format!("a(\"{}\")", s).parse().unwrap()];
            let clauses: Vec<logic::Clause> = vec![
                logic::Clause {
                    head: "a(\"ab\")".parse().unwrap(),
                    body: vec![],
                },
                "a(S) :- string_concat(\"a\", X, S), string_concat(Y, \"b\", X), a(Y)"
                    .parse()
                    .unwrap(),
            ];
            let result = sld(&clauses, &goal, 50).unwrap();
            if is_good {
                assert!(result.is_some());
                let solutions = solutions(&result.unwrap());
                assert_eq!(solutions.len(), 1);
                assert!(contains_ignoring_position(
                    &solutions,
                    &vec![format!("a(\"{}\")", s).parse().unwrap()]
                ));
            } else {
                assert!(result.is_none());
            }
        }
    }
}
