// Miniscript
// Written in 2019 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! # Policy Compiler
//!
//! Optimizing compiler from concrete policies to Miniscript
//!

use std::collections::HashMap;
use std::{cmp, error, f64, fmt};

use miniscript::types::extra_props::MAX_OPS_PER_SCRIPT;
use miniscript::types::{self, ErrorKind, ExtData, Property, Type};
use policy::Concrete;
use std::collections::vec_deque::VecDeque;
use std::hash;
use std::sync::Arc;
use Terminal;
use {Miniscript, MiniscriptKey};

///Ordered f64 for comparison
#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
struct OrdF64(f64);

impl Eq for OrdF64 {}
impl Ord for OrdF64 {
    fn cmp(&self, other: &OrdF64) -> cmp::Ordering {
        // will panic if given NaN
        self.0.partial_cmp(&other.0).unwrap()
    }
}

/// Detailed Error type for Compiler
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CompilerError {
    /// Compiler has non-safe input policy.
    TopLevelNonSafe,
    /// Non-Malleable compilation  does exists for the given sub-policy.
    ImpossibleNonMalleableCompilation,
    /// Atleast one satisfaction path in the optimal Miniscript has opcodes
    /// more than `MAX_OPS_PER_SCRIPT`(201). However, there may exist other
    /// miniscripts which are under `MAX_OPS_PER_SCRIPT` but the compiler
    /// currently does not find them.
    MaxOpCountExceeded,
}

impl error::Error for CompilerError {
    fn cause(&self) -> Option<&error::Error> {
        None
    }

    fn description(&self) -> &str {
        ""
    }
}

impl fmt::Display for CompilerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            CompilerError::TopLevelNonSafe => {
                f.write_str("Top Level script is not safe on some spendpath")
            }
            CompilerError::ImpossibleNonMalleableCompilation => {
                f.write_str("The compiler could not find any non-malleable compilation")
            }
            CompilerError::MaxOpCountExceeded => f.write_str(
                "Atleast one spending path has more op codes executed than \
                 MAX_OPS_PER_SCRIPT",
            ),
        }
    }
}

/// Hash required for using OrdF64 as key for hashmap
impl hash::Hash for OrdF64 {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// Compilation key: This represents the state of the best possible compilation
/// of a given policy(implicitly keyed).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
struct CompilationKey {
    /// The type of the compilation result
    ty: Type,

    /// Whether that result cannot be easily converted into verify form.
    /// This is exactly the opposite of has_verify_form in the data-types.
    /// This is required in cases where it is important to distinguish between
    /// two Compilation of the same-type: one of which is expensive to verify
    /// and the other is not.
    expensive_verify: bool,

    /// The probability of dissatisfaction of the compilation of the policy. Note
    /// that all possible compilations of a (sub)policy have the same sat-prob
    /// and only differ in dissat_prob.
    dissat_prob: Option<OrdF64>,
}

impl CompilationKey {
    /// A Compilation key subtype of another if the type if subtype and other
    /// attributes are equal
    fn is_subtype(self, other: Self) -> bool {
        self.ty.is_subtype(other.ty)
            && self.expensive_verify == other.expensive_verify
            && self.dissat_prob == other.dissat_prob
    }

    /// Helper to create compilation key from components
    fn from_type(ty: Type, expensive_verify: bool, dissat_prob: Option<f64>) -> CompilationKey {
        CompilationKey {
            ty: ty,
            expensive_verify: expensive_verify,
            dissat_prob: dissat_prob.and_then(|x| Some(OrdF64(x))),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct CompilerExtData {
    /// If this node is the direct child of a disjunction, this field must
    /// have the probability of its branch being taken. Otherwise it is ignored.
    /// All functions initialize it to `None`.
    branch_prob: Option<f64>,
    /// The number of bytes needed to satisfy the fragment in segwit format
    /// (total length of all witness pushes, plus their own length prefixes)
    sat_cost: f64,
    /// The number of bytes needed to dissatisfy the fragment in segwit format
    /// (total length of all witness pushes, plus their own length prefixes)
    /// for fragments that can be dissatisfied without failing the script.
    dissat_cost: Option<f64>,
}

impl Property for CompilerExtData {
    fn from_true() -> Self {
        // only used in casts. should never be computed directly
        unreachable!();
    }

    fn from_false() -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: f64::MAX,
            dissat_cost: Some(0.0),
        }
    }

    fn from_pk() -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: 73.0,
            dissat_cost: Some(1.0),
        }
    }

    fn from_pk_h() -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: 73.0 + 34.0,
            dissat_cost: Some(1.0 + 34.0),
        }
    }

    fn from_multi(k: usize, _n: usize) -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: 1.0 + 73.0 * k as f64,
            dissat_cost: Some(1.0 * (k + 1) as f64),
        }
    }

    fn from_hash() -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: 33.0,
            dissat_cost: Some(33.0),
        }
    }

    fn from_time(_t: u32) -> Self {
        CompilerExtData {
            branch_prob: None,
            sat_cost: 0.0,
            dissat_cost: None,
        }
    }

    fn cast_alt(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: self.dissat_cost,
        })
    }

    fn cast_swap(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: self.dissat_cost,
        })
    }

    fn cast_check(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: self.dissat_cost,
        })
    }

    fn cast_dupif(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: 2.0 + self.sat_cost,
            dissat_cost: Some(1.0),
        })
    }

    fn cast_verify(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: None,
        })
    }

    fn cast_nonzero(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: Some(1.0),
        })
    }

    fn cast_zeronotequal(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: self.dissat_cost,
        })
    }

    fn cast_true(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: self.sat_cost,
            dissat_cost: None,
        })
    }

    fn cast_or_i_false(self) -> Result<Self, types::ErrorKind> {
        // never called directly
        unreachable!()
    }

    fn cast_unlikely(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: 2.0 + self.sat_cost,
            dissat_cost: Some(1.0),
        })
    }

    fn cast_likely(self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: 1.0 + self.sat_cost,
            dissat_cost: Some(2.0),
        })
    }

    fn and_b(left: Self, right: Self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: left.sat_cost + right.sat_cost,
            dissat_cost: match (left.dissat_cost, right.dissat_cost) {
                (Some(l), Some(r)) => Some(l + r),
                _ => None,
            },
        })
    }

    fn and_v(left: Self, right: Self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: left.sat_cost + right.sat_cost,
            dissat_cost: None,
        })
    }

    fn or_b(l: Self, r: Self) -> Result<Self, types::ErrorKind> {
        let lprob = l
            .branch_prob
            .expect("BUG: left branch prob must be set for disjunctions");
        let rprob = r
            .branch_prob
            .expect("BUG: right branch prob must be set for disjunctions");
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: lprob * (l.sat_cost + r.dissat_cost.unwrap())
                + rprob * (r.sat_cost + l.dissat_cost.unwrap()),
            dissat_cost: Some(l.dissat_cost.unwrap() + r.dissat_cost.unwrap()),
        })
    }

    fn or_d(l: Self, r: Self) -> Result<Self, types::ErrorKind> {
        let lprob = l
            .branch_prob
            .expect("BUG: left branch prob must be set for disjunctions");
        let rprob = r
            .branch_prob
            .expect("BUG: right branch prob must be set for disjunctions");
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: lprob * l.sat_cost + rprob * (r.sat_cost + l.dissat_cost.unwrap()),
            dissat_cost: r.dissat_cost.map(|rd| l.dissat_cost.unwrap() + rd),
        })
    }

    fn or_c(l: Self, r: Self) -> Result<Self, types::ErrorKind> {
        let lprob = l
            .branch_prob
            .expect("BUG: left branch prob must be set for disjunctions");
        let rprob = r
            .branch_prob
            .expect("BUG: right branch prob must be set for disjunctions");
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: lprob * l.sat_cost + rprob * (r.sat_cost + l.dissat_cost.unwrap()),
            dissat_cost: None,
        })
    }

    fn or_i(l: Self, r: Self) -> Result<Self, types::ErrorKind> {
        let lprob = l
            .branch_prob
            .expect("BUG: left branch prob must be set for disjunctions");
        let rprob = r
            .branch_prob
            .expect("BUG: right branch prob must be set for disjunctions");
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: lprob * (2.0 + l.sat_cost) + rprob * (1.0 + r.sat_cost),
            dissat_cost: if let (Some(ldis), Some(rdis)) = (l.dissat_cost, r.dissat_cost) {
                if (2.0 + ldis) > (1.0 + rdis) {
                    Some(1.0 + rdis)
                } else {
                    Some(2.0 + ldis)
                }
            } else if let Some(ldis) = l.dissat_cost {
                Some(2.0 + ldis)
            } else if let Some(rdis) = r.dissat_cost {
                Some(1.0 + rdis)
            } else {
                None
            },
        })
    }

    fn and_or(a: Self, b: Self, c: Self) -> Result<Self, types::ErrorKind> {
        if a.dissat_cost.is_none() {
            return Err(ErrorKind::LeftNotDissatisfiable);
        }
        let aprob = a.branch_prob.expect("andor, a prob must be set");
        let bprob = b.branch_prob.expect("andor, b prob must be set");
        let cprob = c.branch_prob.expect("andor, c prob must be set");

        let adis = a
            .dissat_cost
            .expect("BUG: and_or first arg(a) must be dissatisfiable");
        debug_assert_eq!(aprob, bprob); //A and B must have same branch prob.
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: aprob * (a.sat_cost + b.sat_cost) + cprob * (adis + c.sat_cost),
            dissat_cost: if let Some(cdis) = c.dissat_cost {
                Some(adis + cdis)
            } else {
                None
            },
        })
    }

    fn and_n(a: Self, b: Self) -> Result<Self, types::ErrorKind> {
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: a.sat_cost + b.sat_cost,
            dissat_cost: a.dissat_cost,
        })
    }

    fn threshold<S>(k: usize, n: usize, mut sub_ck: S) -> Result<Self, types::ErrorKind>
    where
        S: FnMut(usize) -> Result<Self, types::ErrorKind>,
    {
        let k_over_n = k as f64 / n as f64;
        let mut sat_cost = 0.0;
        let mut dissat_cost = 0.0;
        for i in 0..n {
            let sub = sub_ck(i)?;
            sat_cost += sub.sat_cost;
            dissat_cost += sub.dissat_cost.unwrap();
        }
        Ok(CompilerExtData {
            branch_prob: None,
            sat_cost: sat_cost * k_over_n + dissat_cost * (1.0 - k_over_n),
            dissat_cost: Some(dissat_cost),
        })
    }
}

/// Miniscript AST fragment with additional data needed by the compiler
#[derive(Clone, Debug)]
struct AstElemExt<Pk: MiniscriptKey> {
    /// The actual Miniscript fragment with type information
    ms: Arc<Miniscript<Pk>>,
    /// Its "type" in terms of compiler data
    comp_ext_data: CompilerExtData,
}

impl<Pk: MiniscriptKey> AstElemExt<Pk> {
    /// Compute a 1-dimensional cost, given a probability of satisfaction
    /// and a probability of dissatisfaction; if `dissat_prob` is `None`
    /// then it is assumed that dissatisfaction never occurs
    fn cost_1d(&self, sat_prob: f64, dissat_prob: Option<f64>) -> f64 {
        self.ms.ext.pk_cost as f64
            + self.comp_ext_data.sat_cost * sat_prob
            + match (dissat_prob, self.comp_ext_data.dissat_cost) {
                (Some(prob), Some(cost)) => prob * cost,
                (Some(_), None) => f64::INFINITY,
                (None, Some(_)) => 0.0,
                (None, None) => 0.0,
            }
    }
}

impl<Pk: MiniscriptKey> AstElemExt<Pk> where {
    fn terminal(ast: Terminal<Pk>) -> AstElemExt<Pk> {
        AstElemExt {
            comp_ext_data: CompilerExtData::type_check(&ast, |_| None).unwrap(),
            ms: Arc::new(Miniscript::from_ast(ast).expect("Terminal creation must always succeed")),
        }
    }

    fn binary(
        ast: Terminal<Pk>,
        l: &AstElemExt<Pk>,
        r: &AstElemExt<Pk>,
    ) -> Result<AstElemExt<Pk>, types::Error<Pk>> {
        let lookup_ext = |n| match n {
            0 => Some(l.comp_ext_data),
            1 => Some(r.comp_ext_data),
            _ => unreachable!(),
        };
        //Types and ExtData are already cached and stored in children. So, we can
        //type_check without cache. For Compiler extra data, we supply a cache.
        let ty = types::Type::type_check(&ast, |_| None)?;
        let ext = types::ExtData::type_check(&ast, |_| None)?;
        let comp_ext_data = CompilerExtData::type_check(&ast, lookup_ext)?;
        Ok(AstElemExt {
            ms: Arc::new(Miniscript {
                ty: ty,
                ext: ext,
                node: ast,
            }),
            comp_ext_data: comp_ext_data,
        })
    }

    fn ternary(
        ast: Terminal<Pk>,
        a: &AstElemExt<Pk>,
        b: &AstElemExt<Pk>,
        c: &AstElemExt<Pk>,
    ) -> Result<AstElemExt<Pk>, types::Error<Pk>> {
        let lookup_ext = |n| match n {
            0 => Some(a.comp_ext_data),
            1 => Some(b.comp_ext_data),
            2 => Some(c.comp_ext_data),
            _ => unreachable!(),
        };
        //Types and ExtData are already cached and stored in children. So, we can
        //type_check without cache. For Compiler extra data, we supply a cache.
        let ty = types::Type::type_check(&ast, |_| None)?;
        let ext = types::ExtData::type_check(&ast, |_| None)?;
        let comp_ext_data = CompilerExtData::type_check(&ast, lookup_ext)?;
        Ok(AstElemExt {
            ms: Arc::new(Miniscript {
                ty: ty,
                ext: ext,
                node: ast,
            }),
            comp_ext_data: comp_ext_data,
        })
    }
}

/// Different types of casts possible for each node.
#[derive(Copy, Clone)]
struct Cast<Pk: MiniscriptKey> {
    node: fn(Arc<Miniscript<Pk>>) -> Terminal<Pk>,
    ast_type: fn(types::Type) -> Result<types::Type, ErrorKind>,
    ext_data: fn(types::ExtData) -> Result<types::ExtData, ErrorKind>,
    comp_ext_data: fn(CompilerExtData) -> Result<CompilerExtData, types::ErrorKind>,
}

impl<Pk: MiniscriptKey> Cast<Pk> {
    fn cast(&self, ast: &AstElemExt<Pk>) -> Result<AstElemExt<Pk>, ErrorKind> {
        Ok(AstElemExt {
            ms: Arc::new(Miniscript {
                ty: (self.ast_type)(ast.ms.ty)?,
                ext: (self.ext_data)(ast.ms.ext)?,
                node: (self.node)(Arc::clone(&ast.ms)),
            }),
            comp_ext_data: (self.comp_ext_data)(ast.comp_ext_data)?,
        })
    }
}

fn all_casts<Pk: MiniscriptKey>() -> [Cast<Pk>; 10] {
    [
        Cast {
            ext_data: types::ExtData::cast_check,
            node: Terminal::Check,
            ast_type: types::Type::cast_check,
            comp_ext_data: CompilerExtData::cast_check,
        },
        Cast {
            ext_data: types::ExtData::cast_dupif,
            node: Terminal::DupIf,
            ast_type: types::Type::cast_dupif,
            comp_ext_data: CompilerExtData::cast_dupif,
        },
        Cast {
            ext_data: types::ExtData::cast_likely,
            node: |ms| {
                Terminal::OrI(
                    Arc::new(
                        Miniscript::from_ast(Terminal::False).expect("False Miniscript creation"),
                    ),
                    ms,
                )
            },
            ast_type: types::Type::cast_likely,
            comp_ext_data: CompilerExtData::cast_likely,
        },
        Cast {
            ext_data: types::ExtData::cast_unlikely,
            node: |ms| {
                Terminal::OrI(
                    ms,
                    Arc::new(
                        Miniscript::from_ast(Terminal::False).expect("False Miniscript creation"),
                    ),
                )
            },
            ast_type: types::Type::cast_unlikely,
            comp_ext_data: CompilerExtData::cast_unlikely,
        },
        Cast {
            ext_data: types::ExtData::cast_verify,
            node: Terminal::Verify,
            ast_type: types::Type::cast_verify,
            comp_ext_data: CompilerExtData::cast_verify,
        },
        Cast {
            ext_data: types::ExtData::cast_nonzero,
            node: Terminal::NonZero,
            ast_type: types::Type::cast_nonzero,
            comp_ext_data: CompilerExtData::cast_nonzero,
        },
        Cast {
            ext_data: types::ExtData::cast_true,
            node: |ms| {
                Terminal::AndV(
                    ms,
                    Arc::new(
                        Miniscript::from_ast(Terminal::True).expect("True Miniscript creation"),
                    ),
                )
            },
            ast_type: types::Type::cast_true,
            comp_ext_data: CompilerExtData::cast_true,
        },
        Cast {
            ext_data: types::ExtData::cast_swap,
            node: Terminal::Swap,
            ast_type: types::Type::cast_swap,
            comp_ext_data: CompilerExtData::cast_swap,
        },
        Cast {
            node: Terminal::Alt,
            ast_type: types::Type::cast_alt,
            ext_data: types::ExtData::cast_alt,
            comp_ext_data: CompilerExtData::cast_alt,
        },
        Cast {
            ext_data: types::ExtData::cast_zeronotequal,
            node: Terminal::ZeroNotEqual,
            ast_type: types::Type::cast_zeronotequal,
            comp_ext_data: CompilerExtData::cast_zeronotequal,
        },
    ]
}

/// Insert an element into the global map and return whether it got inserted
/// If there is any element which is already better than current element
/// (by subtyping rules), then don't process the element and return `False`.
/// Otherwise, if the element got inserted into the map, return `True` to inform
/// the caller that the cast closure of this element must also be inserted into
/// the map.
/// In general, we maintain the invariant that if anything is inserted into the
/// map, it's cast closure must also be considered for best compilations.
fn insert_elem<Pk: MiniscriptKey>(
    map: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    elem: AstElemExt<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> bool {
    // return malleable types directly. If a elem is malleable, all the casts
    // to it are also going to be malleable
    if !elem.ms.ty.mall.non_malleable {
        return false;
    }
    if let Some(op_count) = elem.ms.ext.ops_count_sat {
        if op_count > MAX_OPS_PER_SCRIPT {
            return false;
        }
    }

    let elem_cost = elem.cost_1d(sat_prob, dissat_prob);

    let elem_key = CompilationKey::from_type(elem.ms.ty, elem.ms.ext.has_verify_form, dissat_prob);

    // Check whether the new element is worse than any existing element. If there
    // is an element which is a subtype of the current element and has better
    // cost, don't consider this element.
    let is_worse = map
        .iter()
        .map(|(existing_key, existing_elem)| {
            let existing_elem_cost = existing_elem.cost_1d(sat_prob, dissat_prob);
            existing_key.is_subtype(elem_key) && existing_elem_cost <= elem_cost
        })
        .fold(false, |acc, x| acc || x);
    if !is_worse {
        // If the element is not worse any element in the map, remove elements
        // whose subtype is the current element and have worse cost.
        map.retain(|&existing_key, existing_elem| {
            let existing_elem_cost = existing_elem.cost_1d(sat_prob, dissat_prob);
            !(elem_key.is_subtype(existing_key) && existing_elem_cost >= elem_cost)
        });
        map.insert(elem_key, elem);
    }
    !is_worse
}

/// Insert the cast-closure of  in the `astelem_ext`. The cast_stack
/// has all the elements whose closure is yet to inserted in the map.
/// A cast-closure refers to trying all possible casts on a particular element
/// if they are better than the current elements in the global map.
///
/// At the start and end of this function, we maintain that the invariant that
/// all map is smallest possible closure of all compilations of a policy with
/// given sat and dissat probabilities.
fn insert_elem_closure<Pk: MiniscriptKey>(
    map: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    astelem_ext: AstElemExt<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) {
    let mut cast_stack: VecDeque<AstElemExt<Pk>> = VecDeque::new();
    if insert_elem(map, astelem_ext.clone(), sat_prob, dissat_prob) {
        cast_stack.push_back(astelem_ext);
    }

    let casts: [Cast<Pk>; 10] = all_casts::<Pk>();
    while !cast_stack.is_empty() {
        let current = cast_stack.pop_front().unwrap();

        for i in 0..casts.len() {
            if let Ok(new_ext) = casts[i].cast(&current) {
                if insert_elem(map, new_ext.clone(), sat_prob, dissat_prob) {
                    cast_stack.push_back(new_ext);
                }
            }
        }
    }
}

/// Insert the best wrapped compilations of a particular Terminal. If the
/// dissat probability is None, then we directly get the closure of the element
/// Otherwise, some wrappers require the compilation of the policy with dissat
/// `None` because they convert it into a dissat around it.
/// For example, `l` wrapper should it argument it dissat. `None` because it can
/// always dissatisfy the policy outside and it find the better inner compilation
/// given that it may be not be necessary to dissatisfy. For these elements, we
/// apply the wrappers around the element once and bring them into the same
/// dissat probability map and get their closure.
fn insert_best_wrapped<Pk: MiniscriptKey>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    map: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    data: AstElemExt<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<(), CompilerError> {
    insert_elem_closure(map, data, sat_prob, dissat_prob);

    if dissat_prob.is_some() {
        let casts: [Cast<Pk>; 10] = all_casts::<Pk>();

        for i in 0..casts.len() {
            for x in best_compilations(policy_cache, policy, sat_prob, None)?.values() {
                if let Ok(new_ext) = casts[i].cast(x) {
                    insert_elem_closure(map, new_ext, sat_prob, dissat_prob);
                }
            }
        }
    }
    Ok(())
}

/// Get the best compilations of a policy with a given sat and dissat
/// probabilities. This functions caches the results into a global policy cache.
fn best_compilations<Pk>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<HashMap<CompilationKey, AstElemExt<Pk>>, CompilerError>
where
    Pk: MiniscriptKey,
{
    //Check the cache for hits
    let ord_sat_prob = OrdF64(sat_prob);
    let ord_dissat_prob = dissat_prob.and_then(|x| Some(OrdF64(x)));
    if let Some(ret) = policy_cache.get(&(policy.clone(), ord_sat_prob, ord_dissat_prob)) {
        return Ok(ret.clone());
    }

    let mut ret = HashMap::new();

    //handy macro for good looking code
    macro_rules! insert_wrap {
        ($x:expr) => {
            insert_best_wrapped(policy_cache, policy, &mut ret, $x, sat_prob, dissat_prob)?
        };
    }
    macro_rules! compile_binary {
        ($l:expr, $r:expr, $w: expr, $f: expr) => {
            compile_binary(
                policy_cache,
                policy,
                &mut ret,
                $l,
                $r,
                $w,
                sat_prob,
                dissat_prob,
                $f,
            )?
        };
    }
    macro_rules! compile_tern {
        ($a:expr, $b:expr, $c: expr, $w: expr) => {
            compile_tern(
                policy_cache,
                policy,
                &mut ret,
                $a,
                $b,
                $c,
                $w,
                sat_prob,
                dissat_prob,
            )?
        };
    }

    match *policy {
        Concrete::Key(ref pk) => {
            insert_wrap!(AstElemExt::terminal(Terminal::PkH(
                pk.to_pubkeyhash().clone()
            )));
            insert_wrap!(AstElemExt::terminal(Terminal::Pk(pk.clone())));
        }
        Concrete::After(n) => insert_wrap!(AstElemExt::terminal(Terminal::After(n))),
        Concrete::Older(n) => insert_wrap!(AstElemExt::terminal(Terminal::Older(n))),
        Concrete::Sha256(hash) => insert_wrap!(AstElemExt::terminal(Terminal::Sha256(hash))),
        Concrete::Hash256(hash) => insert_wrap!(AstElemExt::terminal(Terminal::Hash256(hash))),
        Concrete::Ripemd160(hash) => insert_wrap!(AstElemExt::terminal(Terminal::Ripemd160(hash))),
        Concrete::Hash160(hash) => insert_wrap!(AstElemExt::terminal(Terminal::Hash160(hash))),
        Concrete::And(ref subs) => {
            assert_eq!(subs.len(), 2, "and takes 2 args");
            let mut left = best_compilations(policy_cache, &subs[0], sat_prob, dissat_prob)?;
            let mut right = best_compilations(policy_cache, &subs[1], sat_prob, dissat_prob)?;
            let mut q_zero_right = best_compilations(policy_cache, &subs[1], sat_prob, None)?;
            let mut q_zero_left = best_compilations(policy_cache, &subs[0], sat_prob, None)?;

            compile_binary!(&mut left, &mut right, [1.0, 1.0], Terminal::AndB);
            compile_binary!(&mut right, &mut left, [1.0, 1.0], Terminal::AndB);
            compile_binary!(&mut left, &mut right, [1.0, 1.0], Terminal::AndV);
            compile_binary!(&mut right, &mut left, [1.0, 1.0], Terminal::AndV);
            let mut zero_comp = HashMap::new();
            zero_comp.insert(
                CompilationKey::from_type(
                    Type::from_false(),
                    ExtData::from_false().has_verify_form,
                    dissat_prob,
                ),
                AstElemExt::terminal(Terminal::False),
            );
            compile_tern!(&mut left, &mut q_zero_right, &mut zero_comp, [1.0, 0.0]);
            compile_tern!(&mut right, &mut q_zero_left, &mut zero_comp, [1.0, 0.0]);
        }
        Concrete::Or(ref subs) => {
            let total = (subs[0].0 + subs[1].0) as f64;
            let lw = subs[0].0 as f64 / total;
            let rw = subs[1].0 as f64 / total;

            //and-or
            if let (&Concrete::And(ref x), _) = (&subs[0].1, &subs[1].1) {
                let mut a1 = best_compilations(
                    policy_cache,
                    &x[0],
                    lw * sat_prob,
                    Some(dissat_prob.unwrap_or(0 as f64) + rw * sat_prob),
                )?;
                let mut a2 = best_compilations(policy_cache, &x[0], lw * sat_prob, None)?;

                let mut b1 = best_compilations(
                    policy_cache,
                    &x[1],
                    lw * sat_prob,
                    Some(dissat_prob.unwrap_or(0 as f64) + rw * sat_prob),
                )?;
                let mut b2 = best_compilations(policy_cache, &x[1], lw * sat_prob, None)?;

                let mut c =
                    best_compilations(policy_cache, &subs[1].1, rw * sat_prob, dissat_prob)?;

                compile_tern!(&mut a1, &mut b2, &mut c, [lw, rw]);
                compile_tern!(&mut b1, &mut a2, &mut c, [lw, rw]);
            };
            if let (_, &Concrete::And(ref x)) = (&subs[0].1, &subs[1].1) {
                let mut a1 = best_compilations(
                    policy_cache,
                    &x[0],
                    rw * sat_prob,
                    Some(dissat_prob.unwrap_or(0 as f64) + lw * sat_prob),
                )?;
                let mut a2 = best_compilations(policy_cache, &x[0], rw * sat_prob, None)?;

                let mut b1 = best_compilations(
                    policy_cache,
                    &x[1],
                    rw * sat_prob,
                    Some(dissat_prob.unwrap_or(0 as f64) + lw * sat_prob),
                )?;
                let mut b2 = best_compilations(policy_cache, &x[1], rw * sat_prob, None)?;

                let mut c =
                    best_compilations(policy_cache, &subs[0].1, lw * sat_prob, dissat_prob)?;

                compile_tern!(&mut a1, &mut b2, &mut c, [rw, lw]);
                compile_tern!(&mut b1, &mut a2, &mut c, [rw, lw]);
            };

            let dissat_probs = |w: f64| -> Vec<Option<f64>> {
                let mut dissat_set = Vec::new();
                dissat_set.push(Some(dissat_prob.unwrap_or(0 as f64) + w * sat_prob));
                dissat_set.push(Some(w * sat_prob));
                dissat_set.push(dissat_prob);
                dissat_set.push(None);
                dissat_set
            };

            let mut l_comp = vec![];
            let mut r_comp = vec![];

            for dissat_prob in dissat_probs(rw).iter() {
                let l = best_compilations(policy_cache, &subs[0].1, lw * sat_prob, *dissat_prob)?;
                l_comp.push(l);
            }

            for dissat_prob in dissat_probs(lw).iter() {
                let r = best_compilations(policy_cache, &subs[1].1, rw * sat_prob, *dissat_prob)?;
                r_comp.push(r);
            }
            compile_binary!(&mut l_comp[0], &mut r_comp[0], [lw, rw], Terminal::OrB);
            compile_binary!(&mut r_comp[0], &mut l_comp[0], [rw, lw], Terminal::OrB);

            compile_binary!(&mut l_comp[0], &mut r_comp[2], [lw, rw], Terminal::OrD);
            compile_binary!(&mut r_comp[0], &mut l_comp[2], [rw, lw], Terminal::OrD);

            compile_binary!(&mut l_comp[1], &mut r_comp[3], [lw, rw], Terminal::OrC);
            compile_binary!(&mut r_comp[1], &mut l_comp[3], [rw, lw], Terminal::OrC);

            compile_binary!(&mut l_comp[2], &mut r_comp[3], [lw, rw], Terminal::OrI);
            compile_binary!(&mut r_comp[2], &mut l_comp[3], [rw, lw], Terminal::OrI);

            compile_binary!(&mut l_comp[3], &mut r_comp[2], [lw, rw], Terminal::OrI);
            compile_binary!(&mut r_comp[3], &mut l_comp[2], [rw, lw], Terminal::OrI);
        }
        Concrete::Threshold(k, ref subs) => {
            let n = subs.len();
            let k_over_n = k as f64 / n as f64;

            let mut sub_ast = Vec::with_capacity(n);
            let mut sub_ext_data = Vec::with_capacity(n);

            let mut best_es = Vec::with_capacity(n);
            let mut best_ws = Vec::with_capacity(n);

            let mut min_value = (0 as usize, f64::INFINITY as f64);
            for (i, ast) in subs.iter().enumerate() {
                let sp = sat_prob * k_over_n;
                //Expressions must be dissatisfiable
                let dp = Some(dissat_prob.unwrap_or(0 as f64) + (1.0 - k_over_n) * sat_prob);
                let be = best_e(policy_cache, ast, sp, dp)?;
                let bw = best_w(policy_cache, ast, sp, dp)?;

                let diff = be.cost_1d(sp, dp) - bw.cost_1d(sp, dp);
                best_es.push((be.comp_ext_data, be));
                best_ws.push((bw.comp_ext_data, bw));

                if diff < min_value.1 {
                    min_value.0 = i;
                    min_value.1 = diff;
                }
            }
            sub_ext_data.push(best_es[min_value.0].0);
            sub_ast.push(Arc::clone(&best_es[min_value.0].1.ms));
            for (i, _ast) in subs.iter().enumerate() {
                if i != min_value.0 {
                    sub_ext_data.push(best_ws[i].0);
                    sub_ast.push(Arc::clone(&best_ws[i].1.ms));
                }
            }

            let ast = Terminal::Thresh(k, sub_ast);
            let ast_ext = AstElemExt {
                ms: Arc::new(
                    Miniscript::from_ast(ast)
                        .expect("threshold subs, which we just compiled, typeck"),
                ),
                comp_ext_data: CompilerExtData::threshold(k, n, |i| Ok(sub_ext_data[i]))
                    .expect("threshold subs, which we just compiled, typeck"),
            };
            insert_wrap!(ast_ext);

            let key_vec: Vec<Pk> = subs
                .iter()
                .filter_map(|s| {
                    if let Concrete::Key(ref pk) = *s {
                        Some(pk.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if key_vec.len() == subs.len() && subs.len() <= 20 {
                insert_wrap!(AstElemExt::terminal(Terminal::ThreshM(k, key_vec)));
            }
        }
    }
    for k in ret.keys() {
        debug_assert_eq!(k.dissat_prob, ord_dissat_prob);
    }
    if ret.len() == 0 {
        // The only reason we are discarding elements out of compiler is because
        // compilations exceed opcount or are non-malleable . If there no possible
        // compilations for any policies regardless of dissat probability then it
        // must have all compilations exceeded the Max Opcount because we already
        // checked that policy must have non-malleable compilations before calling
        // this compile function
        Err(CompilerError::MaxOpCountExceeded)
    } else {
        policy_cache.insert((policy.clone(), ord_sat_prob, ord_dissat_prob), ret.clone());
        Ok(ret)
    }
}

/// Helper function to compile different types of binary fragments.
/// `sat_prob` and `dissat_prob` represent the sat and dissat probabilities of
/// root or. `weights` represent the odds for taking each sub branch
fn compile_binary<Pk, F>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    ret: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    left_comp: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    right_comp: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    weights: [f64; 2],
    sat_prob: f64,
    dissat_prob: Option<f64>,
    bin_func: F,
) -> Result<(), CompilerError>
where
    Pk: MiniscriptKey,
    F: Fn(Arc<Miniscript<Pk>>, Arc<Miniscript<Pk>>) -> Terminal<Pk>,
{
    for l in left_comp.values_mut() {
        let lref = Arc::clone(&l.ms);
        for r in right_comp.values_mut() {
            let rref = Arc::clone(&r.ms);
            let ast = bin_func(Arc::clone(&lref), Arc::clone(&rref));
            l.comp_ext_data.branch_prob = Some(weights[0]);
            r.comp_ext_data.branch_prob = Some(weights[1]);
            if let Ok(new_ext) = AstElemExt::binary(ast, l, r) {
                insert_best_wrapped(policy_cache, policy, ret, new_ext, sat_prob, dissat_prob)?;
            }
        }
    }
    Ok(())
}

/// Helper function to compile different order of and_or fragments.
/// `sat_prob` and `dissat_prob` represent the sat and dissat probabilities of
/// root and_or node. `weights` represent the odds for taking each sub branch
fn compile_tern<Pk: MiniscriptKey>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    ret: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    a_comp: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    b_comp: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    c_comp: &mut HashMap<CompilationKey, AstElemExt<Pk>>,
    weights: [f64; 2],
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<(), CompilerError> {
    for a in a_comp.values_mut() {
        let aref = Arc::clone(&a.ms);
        for b in b_comp.values_mut() {
            let bref = Arc::clone(&b.ms);
            for c in c_comp.values_mut() {
                let cref = Arc::clone(&c.ms);
                let ast = Terminal::AndOr(Arc::clone(&aref), Arc::clone(&bref), Arc::clone(&cref));
                a.comp_ext_data.branch_prob = Some(weights[0]);
                b.comp_ext_data.branch_prob = Some(weights[0]);
                c.comp_ext_data.branch_prob = Some(weights[1]);
                if let Ok(new_ext) = AstElemExt::ternary(ast, a, b, c) {
                    insert_best_wrapped(policy_cache, policy, ret, new_ext, sat_prob, dissat_prob)?;
                }
            }
        }
    }
    Ok(())
}

/// Obtain the best compilation of for p=1.0 and q=0
pub fn best_compilation<Pk: MiniscriptKey>(
    policy: &Concrete<Pk>,
) -> Result<Miniscript<Pk>, CompilerError> {
    let mut policy_cache = HashMap::new();
    let x = &*best_t(&mut policy_cache, policy, 1.0, None)?.ms;
    if !x.ty.mall.safe {
        Err(CompilerError::TopLevelNonSafe)
    } else if !x.ty.mall.non_malleable {
        Err(CompilerError::ImpossibleNonMalleableCompilation)
    } else {
        Ok(x.clone())
    }
}

/// Obtain the best B expression with given sat and dissat
fn best_t<Pk>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<AstElemExt<Pk>, CompilerError>
where
    Pk: MiniscriptKey,
{
    best_compilations(policy_cache, policy, sat_prob, dissat_prob)?
        .into_iter()
        .filter(|&(key, _)| {
            key.ty.corr.base == types::Base::B
                && key.dissat_prob == dissat_prob.and_then(|x| Some(OrdF64(x)))
        })
        .map(|(_, val)| val)
        .min_by_key(|ext| OrdF64(ext.cost_1d(sat_prob, dissat_prob)))
        .ok_or(CompilerError::MaxOpCountExceeded)
}

/// Obtain the B.deu expression with the given sat and dissat
fn best_e<Pk>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<AstElemExt<Pk>, CompilerError>
where
    Pk: MiniscriptKey,
{
    best_compilations(policy_cache, policy, sat_prob, dissat_prob)?
        .into_iter()
        .filter(|&(ref key, ref val)| {
            key.ty.corr.base == types::Base::B
                && key.ty.corr.unit
                && val.ms.ty.mall.dissat == types::Dissat::Unique
                && key.dissat_prob == dissat_prob.and_then(|x| Some(OrdF64(x)))
        })
        .map(|(_, val)| val)
        .min_by_key(|ext| OrdF64(ext.cost_1d(sat_prob, dissat_prob)))
        .ok_or(CompilerError::MaxOpCountExceeded)
}

/// Obtain the W.deu expression with the given sat and dissat
fn best_w<Pk>(
    policy_cache: &mut HashMap<
        (Concrete<Pk>, OrdF64, Option<OrdF64>),
        HashMap<CompilationKey, AstElemExt<Pk>>,
    >,
    policy: &Concrete<Pk>,
    sat_prob: f64,
    dissat_prob: Option<f64>,
) -> Result<AstElemExt<Pk>, CompilerError>
where
    Pk: MiniscriptKey,
{
    best_compilations(policy_cache, policy, sat_prob, dissat_prob)?
        .into_iter()
        .filter(|&(ref key, ref val)| {
            key.ty.corr.base == types::Base::W
                && key.ty.corr.unit
                && val.ms.ty.mall.dissat == types::Dissat::Unique
                && key.dissat_prob == dissat_prob.and_then(|x| Some(OrdF64(x)))
        })
        .map(|(_, val)| val)
        .min_by_key(|ext| OrdF64(ext.cost_1d(sat_prob, dissat_prob)))
        .ok_or(CompilerError::MaxOpCountExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::blockdata::{opcodes, script};
    use bitcoin::{self, hashes, secp256k1, SigHashType};
    use std::str::FromStr;

    use miniscript::satisfy;
    use policy::Liftable;
    use BitcoinSig;
    use DummyKey;

    type SPolicy = Concrete<String>;
    type DummyPolicy = Concrete<DummyKey>;
    type BPolicy = Concrete<bitcoin::PublicKey>;

    fn pubkeys_and_a_sig(n: usize) -> (Vec<bitcoin::PublicKey>, secp256k1::Signature) {
        let mut ret = Vec::with_capacity(n);
        let secp = secp256k1::Secp256k1::new();
        let mut sk = [0; 32];
        for i in 1..n + 1 {
            sk[0] = i as u8;
            sk[1] = (i >> 8) as u8;
            sk[2] = (i >> 16) as u8;

            let pk = bitcoin::PublicKey {
                key: secp256k1::PublicKey::from_secret_key(
                    &secp,
                    &secp256k1::SecretKey::from_slice(&sk[..]).expect("sk"),
                ),
                compressed: true,
            };
            ret.push(pk);
        }
        let sig = secp.sign(
            &secp256k1::Message::from_slice(&sk[..]).expect("secret key"),
            &secp256k1::SecretKey::from_slice(&sk[..]).expect("secret key"),
        );
        (ret, sig)
    }

    fn policy_compile_lift_check(s: &str) -> Result<(), CompilerError> {
        let policy = DummyPolicy::from_str(s).expect("parse");
        let miniscript = policy.compile()?;

        assert_eq!(policy.lift().sorted(), miniscript.lift().sorted());
        Ok(())
    }

    #[test]
    fn compile_basic() {
        assert!(policy_compile_lift_check("pk()").is_ok());
        assert_eq!(
            policy_compile_lift_check("after(9)"),
            Err(CompilerError::TopLevelNonSafe)
        );
        assert_eq!(
            policy_compile_lift_check("older(1)"),
            Err(CompilerError::TopLevelNonSafe)
        );
        assert_eq!(
            policy_compile_lift_check(
                "sha256(1111111111111111111111111111111111111111111111111111111111111111)"
            ),
            Err(CompilerError::TopLevelNonSafe)
        );
        assert!(policy_compile_lift_check("and(pk(),pk())").is_ok());
        assert!(policy_compile_lift_check("or(pk(),pk())").is_ok());
        assert!(policy_compile_lift_check("thresh(2,pk(),pk(),pk())").is_ok());

        assert_eq!(
            policy_compile_lift_check("thresh(2,after(9),after(9),pk())"),
            Err(CompilerError::TopLevelNonSafe)
        );

        assert_eq!(
            policy_compile_lift_check("and(pk(),or(after(9),after(9)))"),
            Err(CompilerError::ImpossibleNonMalleableCompilation)
        );
    }

    #[test]
    fn compile_q() {
        let policy = SPolicy::from_str("or(1@and(pk(),pk()),127@pk())").expect("parsing");
        let compilation = best_t(&mut HashMap::new(), &policy, 1.0, None).unwrap();

        assert_eq!(compilation.cost_1d(1.0, None), 88.0 + 74.109375);
        assert_eq!(policy.lift().sorted(), compilation.ms.lift().sorted());

        let policy = SPolicy::from_str(
                "and(and(and(or(127@thresh(2,pk(),pk(),thresh(2,or(127@pk(),1@pk()),after(100),or(and(pk(),after(200)),and(pk(),sha256(66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925))),pk())),1@pk()),sha256(66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925)),or(127@pk(),1@after(300))),or(127@after(400),pk()))"
            ).expect("parsing");
        let compilation = best_t(&mut HashMap::new(), &policy, 1.0, None).unwrap();

        assert_eq!(compilation.cost_1d(1.0, None), 437.0 + 299.4003295898438);
        assert_eq!(policy.lift().sorted(), compilation.ms.lift().sorted());
    }

    #[test]
    fn compile_misc() {
        let (keys, sig) = pubkeys_and_a_sig(10);
        let key_pol: Vec<BPolicy> = keys.iter().map(|k| Concrete::Key(*k)).collect();

        let policy: BPolicy = Concrete::Key(keys[0].clone());
        let desc = policy.compile().unwrap();
        assert_eq!(
            desc.encode(),
            script::Builder::new()
                .push_key(&keys[0])
                .push_opcode(opcodes::all::OP_CHECKSIG)
                .into_script()
        );

        // CSV reordering trick
        let policy: BPolicy = policy_str!(
            "and(older(10000),thresh(2,pk({}),pk({}),pk({})))",
            keys[5],
            keys[6],
            keys[7]
        );
        let desc = policy.compile().unwrap();
        assert_eq!(
            desc.encode(),
            script::Builder::new()
                .push_opcode(opcodes::all::OP_PUSHNUM_2)
                .push_key(&keys[5])
                .push_key(&keys[6])
                .push_key(&keys[7])
                .push_opcode(opcodes::all::OP_PUSHNUM_3)
                .push_opcode(opcodes::all::OP_CHECKMULTISIGVERIFY)
                .push_int(10000)
                .push_opcode(opcodes::all::OP_CSV)
                .into_script()
        );

        // Liquid policy
        let policy: BPolicy = Concrete::Or(vec![
            (127, Concrete::Threshold(3, key_pol[0..5].to_owned())),
            (
                1,
                Concrete::And(vec![
                    Concrete::Older(10000),
                    Concrete::Threshold(2, key_pol[5..8].to_owned()),
                ]),
            ),
        ]);

        let desc = policy.compile().unwrap();

        let ms: Miniscript<bitcoin::PublicKey> = ms_str!(
            "or_d(thresh_m(3,{},{},{},{},{}),\
             and_v(v:thresh(2,c:pk_h({}),\
             ac:pk_h({}),ac:pk_h({})),older(10000)))",
            keys[0],
            keys[1],
            keys[2],
            keys[3],
            keys[4],
            keys[5].to_pubkeyhash(),
            keys[6].to_pubkeyhash(),
            keys[7].to_pubkeyhash()
        );

        assert_eq!(desc, ms);

        let mut abs = policy.lift();
        assert_eq!(abs.n_keys(), 8);
        assert_eq!(abs.minimum_n_keys(), 2);
        abs = abs.at_age(10000);
        assert_eq!(abs.n_keys(), 8);
        assert_eq!(abs.minimum_n_keys(), 2);
        abs = abs.at_age(9999);
        assert_eq!(abs.n_keys(), 5);
        assert_eq!(abs.minimum_n_keys(), 3);
        abs = abs.at_age(0);
        assert_eq!(abs.n_keys(), 5);
        assert_eq!(abs.minimum_n_keys(), 3);

        let bitcoinsig = (sig, SigHashType::All);
        let mut sigvec = sig.serialize_der().to_vec();
        sigvec.push(1); // sighash all

        let no_sat = HashMap::<bitcoin::PublicKey, BitcoinSig>::new();
        let mut left_sat = HashMap::<bitcoin::PublicKey, BitcoinSig>::new();
        let mut right_sat =
            HashMap::<hashes::hash160::Hash, (bitcoin::PublicKey, BitcoinSig)>::new();

        for i in 0..5 {
            left_sat.insert(keys[i], bitcoinsig);
        }
        for i in 5..8 {
            right_sat.insert(keys[i].to_pubkeyhash(), (keys[i], bitcoinsig));
        }

        assert!(desc.satisfy(no_sat).is_none());
        assert!(desc.satisfy(&left_sat).is_some());
        assert!(desc.satisfy((&right_sat, satisfy::Older(10001))).is_some());
        //timelock not met
        assert!(desc.satisfy((&right_sat, satisfy::Older(9999))).is_none());

        assert_eq!(
            desc.satisfy((left_sat, satisfy::Older(9999))).unwrap(),
            vec![
                // sat for left branch
                vec![],
                sigvec.clone(),
                sigvec.clone(),
                sigvec.clone(),
            ]
        );

        assert_eq!(
            desc.satisfy((right_sat, satisfy::Older(10000))).unwrap(),
            vec![
                // sat for right branch
                vec![],
                keys[7].to_bytes(),
                sigvec.clone(),
                keys[6].to_bytes(),
                sigvec.clone(),
                keys[5].to_bytes(),
                // dissat for left branch
                vec![],
                vec![],
                vec![],
                vec![],
            ]
        );
    }
}

#[cfg(all(test, feature = "unstable"))]
mod benches {
    use secp256k1;
    use std::str::FromStr;
    use test::{black_box, Bencher};

    use Concrete;
    use ParseTree;

    #[bench]
    pub fn compile(bh: &mut Bencher) {
        let desc = Concrete::<secp256k1::PublicKey>::from_str(
            "and(thresh(2,and(sha256(),or(sha256(),pk())),pk(),pk(),pk(),sha256()),pkh())",
        )
        .expect("parsing");
        bh.iter(|| {
            let pt = ParseTree::compile(&desc);
            black_box(pt);
        });
    }

    #[bench]
    pub fn compile_large(bh: &mut Bencher) {
        let desc = Concrete::<secp256k1::PublicKey>::from_str(
            "or(pkh(),thresh(9,sha256(),pkh(),pk(),and(or(pkh(),pk()),pk()),time_e(),pk(),pk(),pk(),pk(),and(pk(),pk())))"
        ).expect("parsing");
        bh.iter(|| {
            let pt = ParseTree::compile(&desc);
            black_box(pt);
        });
    }

    #[bench]
    pub fn compile_xlarge(bh: &mut Bencher) {
        let desc = Concrete::<secp256k1::PublicKey>::from_str(
            "or(pk(),thresh(4,pkh(),time_e(),multi(),and(after(),or(pkh(),or(pkh(),and(pkh(),thresh(2,multi(),or(pkh(),and(thresh(5,sha256(),or(pkh(),pkh()),pkh(),pkh(),pkh(),multi(),pkh(),multi(),pk(),pkh(),pk()),pkh())),pkh(),or(and(pkh(),pk()),pk()),after()))))),pkh()))"
        ).expect("parsing");
        bh.iter(|| {
            let pt = ParseTree::compile(&desc);
            black_box(pt);
        });
    }
}
