//! Bounded plans over immutable checked bodies. Cold deferred bodies stay cold.
use crate::{
    Type,
    ir::{LocalStorage, Op, Program},
};
use cyallo_ast::Operator;
use std::collections::HashMap;

pub(super) struct Plan {
    pub function: usize,
    pub body: Vec<Op>,
    pub children: HashMap<usize, Plan>,
    pub cost: u64,
    pub own_cost: u64,
    pub depth: u64,
}
fn primitive(ty: &Type) -> bool {
    matches!(ty, Type::Integer | Type::Boolean)
}
fn plan(program: &Program, function: usize, stack: &mut Vec<usize>) -> Option<Plan> {
    use cyallo_ast::{CompileHint, InlineHint};
    let routine = &program.functions[function];
    let depth = if routine.attributes.inlining == InlineHint::Always {
        8
    } else {
        3
    };
    if stack.len() > depth
        || stack.contains(&function)
        || !routine.is_analyzed()
        || routine.attributes.compilation == CompileHint::AlwaysLazy
        || routine.attributes.inlining == InlineHint::Never
    {
        return None;
    }
    let f = program.functions[function].checked().ok()?;
    if f.generator.is_some() || f.params.iter().any(|r| *r)
        || (!primitive(&f.result) && f.result != Type::Void)
        || !f.registers.iter().all(primitive)
        || f.local_storage
            .iter()
            .any(|s| matches!(s, LocalStorage::Cell(_)))
    {
        return None;
    }
    let mut defined = vec![false; f.registers.len()];
    for storage in f.local_storage.iter().take(f.params.len()) {
        let LocalStorage::Register(r) = storage else {
            return None;
        };
        defined[*r] = true;
    }
    stack.push(function);
    let result = (|| {
        let mut body = vec![];
        let mut children = HashMap::new();
        let mut cost = 0;
        let mut depth = 1;
        let (callee_limit, site_limit) = match f.attributes.inlining {
            InlineHint::Auto => (32, 64),
            InlineHint::Preferred => (64, 128),
            InlineHint::Always => (256, 256),
            InlineHint::Never => unreachable!(),
        };
        for (pc, instruction) in f.code.iter().enumerate().take(callee_limit) {
            let op = &instruction.op;
            cost += 1;
            match op {
                Op::Statement | Op::Nop => {}
                Op::Literal(
                    r,
                    cyallo_ast::Literal::Integer(_) | cyallo_ast::Literal::Boolean(_),
                ) => defined[*r] = true,
                Op::FrameLoad(r, a) | Op::FrameStore(r, a) | Op::Move(r, a) if defined[*a] => {
                    defined[*r] = true
                }
                Op::Unary(r, Operator::Add | Operator::Subtract | Operator::Not, a)
                    if defined[*a] =>
                {
                    defined[*r] = true
                }
                Op::Binary(
                    r,
                    Operator::Add
                    | Operator::Subtract
                    | Operator::Multiply
                    | Operator::Equal
                    | Operator::NotEqual
                    | Operator::Less
                    | Operator::Greater
                    | Operator::LessEqual
                    | Operator::GreaterEqual,
                    a,
                    b,
                ) if defined[*a] && defined[*b] => defined[*r] = true,
                Op::Call(result, to, args) if args.iter().all(|a| defined[*a]) => {
                    let child = plan(program, *to, stack)?;
                    cost += child.cost;
                    depth = depth.max(1 + child.depth);
                    children.insert(pc, child);
                    if let Some(r) = result {
                        defined[*r] = true;
                    }
                }
                Op::Return(result) if result.is_none_or(|r| defined[r]) => {
                    body.push(op.clone());
                    if cost > site_limit {
                        return None;
                    }
                    return Some(Plan {
                        function,
                        own_cost: body.len() as u64,
                        body,
                        children,
                        cost,
                        depth,
                    });
                }
                _ => return None,
            }
            body.push(op.clone());
            if cost > site_limit {
                return None;
            }
        }
        None
    })();
    stack.pop();
    result
}
impl Plan {
    pub fn accounting(&self, totals: &mut std::collections::BTreeMap<usize, (u64, u64)>) {
        let entry = totals.entry(self.function).or_default();
        entry.0 += self.own_cost;
        entry.1 += 1;
        for child in self.children.values() {
            child.accounting(totals);
        }
    }
}
pub(super) fn plans(
    program: &Program,
    function: Option<usize>,
    enabled: bool,
    procedures: bool,
) -> HashMap<(usize, usize), Plan> {
    let mut plans = HashMap::new();
    if !enabled {
        for (fi, f) in program
            .functions
            .iter()
            .enumerate()
            .filter(|(fi, _)| function.is_none_or(|wanted| wanted == *fi))
        {
            if !f.is_analyzed() {
                continue;
            }
            for (pc, instruction) in f.code.iter().enumerate() {
                if let Op::Call(_, to, _) = instruction.op
                    && program.functions[to].attributes.inlining == cyallo_ast::InlineHint::Always
                {
                    program.notice(
                        "unfulfilled",
                        to,
                        format!("inline(always) at caller {fi} pc {pc}: native inlining disabled"),
                    );
                }
            }
        }
        return plans;
    }
    for (fi, f) in program.functions.iter().enumerate() {
        if f.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
            continue;
        }
        if function.is_some_and(|wanted| wanted != fi) {
            continue;
        }
        let embedded = frame_bodies(program, Some(fi), procedures);
        let mut growth = embedded
            .iter()
            .map(|id| program.functions[*id].code.len() as u64)
            .sum::<u64>();
        for (pc, instruction) in f.code.iter().enumerate() {
            if let Op::Call(_, to, _) = &instruction.op {
                let target = &program.functions[*to];
                let limit = match target.attributes.inlining {
                    cyallo_ast::InlineHint::Preferred => 512,
                    cyallo_ast::InlineHint::Always => 1024,
                    _ => 256,
                };
                if let Some(plan) = plan(program, *to, &mut vec![fi])
                    && growth + plan.cost <= limit
                {
                    program.notice(
                        "inline",
                        fi,
                        format!(
                            "pc {pc}: {} -> frameless SSA; {} expanded words, depth {}",
                            target.name, plan.cost, plan.depth
                        ),
                    );
                    growth += plan.cost;
                    plans.insert((fi, pc), plan);
                } else if target.attributes.inlining == cyallo_ast::InlineHint::Always
                    && !embedded.contains(to)
                {
                    program.notice("unfulfilled",*to,format!("inline(always) at caller {} pc {pc}: unsupported body, recursion or hard growth limit",f.name));
                } else {
                    program.notice("inline",fi,format!("pc {pc}: {} remains a call (policy, deferred body or unsupported frameless operations)",target.name));
                }
            }
        }
    }
    plans
}

/// Small procedure bodies retain their real VM frames and all helper boundaries.
/// Embed one shared body in the caller's unit; canonical frames carry the exact
/// source PC and return continuation for each invocation of that body.
pub(super) fn frame_bodies(
    program: &Program,
    function: Option<usize>,
    enabled: bool,
) -> std::collections::BTreeSet<usize> {
    let mut bodies = std::collections::BTreeSet::new();
    let roots: Vec<_> =
        function.map_or_else(|| (0..program.functions.len()).collect(), |fi| vec![fi]);
    for root in roots {
        if program.functions[root].attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
            continue;
        }
        let mut pending = vec![(root, 0)];
        let mut seen = std::collections::HashSet::from([root]);
        let mut growth = 0;
        while let Some((fi, depth)) = pending.pop() {
            let f = &program.functions[fi];
            if !f.is_analyzed() {
                continue;
            }
            for instruction in &f.code {
                let Op::Call(_, to, _) = instruction.op else {
                    continue;
                };
                let callee = &program.functions[to];
                let forced = callee.attributes.inlining == cyallo_ast::InlineHint::Always;
                let (size_limit, growth_limit, depth_limit) = match callee.attributes.inlining {
                    cyallo_ast::InlineHint::Preferred => (64, 512, 3),
                    cyallo_ast::InlineHint::Always => (256, 1024, 8),
                    _ => (32, 256, 3),
                };
                if !seen.insert(to)
                    || depth >= depth_limit
                    || !callee.is_analyzed()
                    || (!forced && (!enabled || callee.kind != crate::ir::RoutineKind::Procedure))
                    || callee.attributes.inlining == cyallo_ast::InlineHint::Never
                    || callee.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy
                {
                    continue;
                }
                let Ok(body) = callee.checked() else {
                    continue;
                };
                if body.code.len() > size_limit
                    || growth + body.code.len() > growth_limit
                    || plan(program, to, &mut vec![root]).is_some()
                {
                    continue;
                }
                growth += body.code.len();
                bodies.insert(to);
                program.notice(
                    "inline",
                    root,
                    format!(
                        "{} -> embedded body with real frames ({} words)",
                        callee.name,
                        body.code.len()
                    ),
                );
                pending.push((to, depth + 1));
            }
        }
    }
    bodies
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forced_embedded_bodies_respect_depth_and_growth_limits() {
        let mut source = String::new();
        for i in 0..10 {
            let next = if i == 9 {
                "n + 1".into()
            } else {
                format!("f{}(n)", i + 1)
            };
            source.push_str(&format!("#[inline(always)]\nfn f{i}(n: i64) -> i64 {{ if n > 0 {{ return {next} }}; return 0 }}\n"));
        }
        source.push_str("std::io::print(f0(1))\n");
        let p = crate::compile(&source).unwrap();
        let bodies = frame_bodies(&p, Some(0), false);
        assert_eq!(bodies.len(), 8);
        assert!(
            bodies
                .iter()
                .map(|fi| p.functions[*fi].code.len())
                .sum::<usize>()
                <= 1024
        );
    }
}
