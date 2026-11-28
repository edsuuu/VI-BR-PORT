//! The loops of a program's control flow, as the walk recognizer needs them:
//! natural loops by their headers, with the blocks that close them and the
//! edges that leave them.

use crate::ir::{self, Program};

/// A natural loop: its header and every block that reaches one of the
/// header's back edges without passing the header.
#[derive(Debug, Clone)]
pub(crate) struct Loop {
    pub header: usize,
    /// Per block of the program: inside the loop.
    pub body: Vec<bool>,
    /// Blocks inside.
    pub size: usize,
    /// Edges out of the loop: (block inside, block outside).
    pub exits: Vec<(usize, usize)>,
}

impl Loop {
    pub fn contains(&self, b: usize) -> bool {
        self.body.get(b).copied().unwrap_or(false)
    }
}

/// The natural loops of `p`, one per header (back edges to one header make one
/// loop), and the dominator tree they were found with.
pub(crate) struct Loops {
    pub loops: Vec<Loop>,
    /// Per block: its position in reverse postorder (`usize::MAX`: unreachable).
    pub index: Vec<usize>,
    idom: Vec<usize>,
}

impl Loops {
    pub fn new(p: &Program) -> Self {
        let order = ir::reverse_postorder(&p.blocks);
        let mut index = vec![usize::MAX; p.blocks.len()];
        for (n, &b) in order.iter().enumerate() {
            index[b] = n;
        }
        let idom = ir::idoms(&p.blocks, &order, &index);
        let mut out = Loops { loops: Vec::new(), index, idom };
        for &h in &order {
            let latches: Vec<usize> = p.blocks[h]
                .preds
                .iter()
                .copied()
                .filter(|&q| out.index[q] != usize::MAX && out.dominates(h, q))
                .collect();
            if latches.is_empty() {
                continue;
            }
            let mut body = vec![false; p.blocks.len()];
            body[h] = true;
            let mut work = latches;
            while let Some(b) = work.pop() {
                if body[b] || out.index[b] == usize::MAX {
                    continue;
                }
                body[b] = true;
                work.extend(p.blocks[b].preds.iter().copied());
            }
            let mut exits = Vec::new();
            for (b, _) in body.iter().enumerate().filter(|(_, inside)| **inside) {
                for s in p.blocks[b].exit.successors() {
                    if !body[s] && !exits.contains(&(b, s)) {
                        exits.push((b, s));
                    }
                }
            }
            exits.sort_unstable();
            let size = body.iter().filter(|&&x| x).count();
            out.loops.push(Loop { header: h, body, size, exits });
        }
        out
    }

    /// Whether block `a` dominates block `b` (both reachable).
    pub fn dominates(&self, a: usize, b: usize) -> bool {
        if self.index[a] == usize::MAX || self.index[b] == usize::MAX {
            return false;
        }
        let mut d = b;
        loop {
            if d == a {
                return true;
            }
            let up = self.idom[d];
            if up == d || up == usize::MAX {
                return false;
            }
            d = up;
        }
    }

    /// The loops holding block `b`, smallest first.
    pub fn around(&self, b: usize) -> Vec<&Loop> {
        let mut out: Vec<&Loop> = self.loops.iter().filter(|l| l.contains(b)).collect();
        out.sort_by_key(|l| l.size);
        out
    }

    /// The header of the innermost loop holding block `b`, if any.
    pub fn innermost(&self, b: usize) -> Option<usize> {
        self.loops.iter().filter(|l| l.contains(b)).min_by_key(|l| l.size).map(|l| l.header)
    }

    /// Whether block `b` heads a loop (some of its edges in are back edges).
    pub fn is_header(&self, b: usize) -> bool {
        self.loops.iter().any(|l| l.header == b)
    }
}
