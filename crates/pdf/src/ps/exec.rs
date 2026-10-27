use super::eps::embedded_fonts;
use super::scanner::Src;
use super::text::encoding_array;
use super::value::{Flow, GState, V, matrix_val, new_dict};
use super::*;

/// The deepest procedure nesting run (deeper recursion stops the program).
const MAX_CALL_DEPTH: usize = 256;

/// The most operands retained on the operand stack.
pub(super) const MAX_STACK: usize = 100_000;

/// Runtime compound payloads, excluding source tokens, graphics, fonts and container overhead.
const MAX_ALLOCATION: usize = 256 * 1024 * 1024;

impl Interp {
    /// Create an interpreter with independent operation and cumulative runtime-payload budgets.
    pub fn new(page: [f64; 4]) -> Interp {
        Interp {
            b: Builder::new(),
            st: vec![],
            dicts: vec![new_dict()],
            gs: GState {
                ctm: Affine::IDENTITY,
                color: [0.0; 3],
                lw: 1.0,
                cap: Cap::Butt,
                join: Join::Miter,
                miter: 10.0,
                dash: None,
                path: BezPath::new(),
                cur: None,
                start: None,
                depth: 1,
                font: None,
            },
            saved: vec![],
            budget: 8_000_000,
            allocation_budget: MAX_ALLOCATION,
            calls: 0,
            font_dir: HashMap::new(),
            embedded: HashMap::new(),
            font_cache: HashMap::new(),
            page,
            unknown: vec![],
        }
    }

    pub fn run(&mut self, data: &[u8]) {
        self.embedded = embedded_fonts(data);
        let mut src = Src { d: data, pos: 0 };
        while let Some(t) = src.token(0) {
            if let Err(Flow::Quit) = self.exec_token(t, &mut src) {
                break;
            }
        }
    }

    pub(super) fn pop(&mut self) -> V {
        self.st.pop().unwrap_or(V::Null)
    }

    pub(super) fn popn(&mut self) -> f64 {
        self.pop().num().unwrap_or(0.0)
    }

    /// Retain one operand if it fits; bulk callers must preflight their complete result.
    pub(super) fn push(&mut self, v: V) {
        if self.st.len() < MAX_STACK {
            self.st.push(v);
        }
    }

    /// Refuse a bulk result before any operands are copied when `n` more slots would exceed the cap.
    pub(super) fn stack_room(&mut self, n: usize) -> Result<(), Flow> {
        if self.st.len().saturating_add(n) <= MAX_STACK {
            return Ok(());
        }
        self.b.skip("PostScript (operand stack limit)");
        Err(Flow::Quit)
    }

    /// Charge before allocating: discarded or overwritten objects do not replenish the budget.
    pub(super) fn allocate(&mut self, bytes: usize) -> Result<(), Flow> {
        match self.allocation_budget.checked_sub(bytes) {
            Some(left) => {
                self.allocation_budget = left;
                Ok(())
            }
            None => {
                self.b.skip("PostScript (allocation limit)");
                Err(Flow::Quit)
            }
        }
    }

    /// Charge runtime value slots by their in-memory size, saturating oversized byte counts.
    pub(super) fn allocate_items(&mut self, n: usize) -> Result<(), Flow> {
        self.allocate(n.saturating_mul(std::mem::size_of::<V>()))
    }

    /// Charge then snapshot array/procedure slots shallowly, returning no items for other values.
    pub(super) fn items(&mut self, v: &V) -> Result<Vec<V>, Flow> {
        self.allocate_items(v.item_count())?;
        Ok(v.items().unwrap_or_default())
    }

    /// Precharge a string's worst-case lossy UTF-8 copy before producing the existing equality key.
    pub(super) fn comparison_key(&mut self, v: &V) -> Result<String, Flow> {
        if let V::Str(s) = v {
            // Lossy UTF-8 replaces each invalid byte with at most three output bytes.
            self.allocate(s.borrow().len().saturating_mul(3))?;
        }
        Ok(v.key())
    }

    /// Charge the six matrix slots before constructing and pushing their array.
    pub(super) fn push_matrix(&mut self, m: Affine) -> Result<(), Flow> {
        self.allocate_items(6)?;
        self.push(matrix_val(m));
        Ok(())
    }

    /// Charge all 256 encoding slots before constructing and pushing their glyph-name array.
    pub(super) fn push_encoding(&mut self, name: &str) -> Result<(), Flow> {
        self.allocate_items(256)?;
        self.push(encoding_array(name));
        Ok(())
    }

    pub(super) fn lookup(&self, k: &str) -> Option<V> {
        self.dicts.iter().rev().find_map(|d| d.borrow().get(k).cloned())
    }

    /// A token read from the file: procedures are pushed, names executed.
    fn exec_token(&mut self, t: V, src: &mut Src) -> Result<(), Flow> {
        match t {
            V::Name(n) => self.exec_name(&n, src),
            other => {
                self.push(other);
                Ok(())
            }
        }
    }

    /// Execute a value as an object (procedures run).
    pub(super) fn exec_obj(&mut self, v: V, src: &mut Src) -> Result<(), Flow> {
        match v {
            V::Proc(p) => {
                if self.calls >= MAX_CALL_DEPTH {
                    self.b.skip("PostScript (recursion limit)");
                    return Err(Flow::Quit);
                }
                self.calls += 1;
                let mut r = Ok(());
                for x in p.iter() {
                    match x {
                        V::Name(n) => {
                            r = self.exec_name(n, src);
                            if r.is_err() {
                                break;
                            }
                        }
                        V::Op(op) => {
                            r = self.spend(1).and_then(|()| self.builtin(op, src));
                            if r.is_err() {
                                break;
                            }
                        }
                        // Other items count against the budget as names do.
                        other => {
                            r = self.spend(1);
                            if r.is_err() {
                                break;
                            }
                            self.push(other.clone());
                        }
                    }
                }
                self.calls -= 1;
                r
            }
            // An executable name can run another (`exec` of `/exec cvx`), so names count too.
            V::Name(n) => {
                if self.calls >= MAX_CALL_DEPTH {
                    self.b.skip("PostScript (recursion limit)");
                    return Err(Flow::Quit);
                }
                self.calls += 1;
                let r = self.exec_name(&n, src);
                self.calls -= 1;
                r
            }
            V::Op(op) => {
                self.spend(1)?;
                self.builtin(&op, src)
            }
            other => {
                self.push(other);
                Ok(())
            }
        }
    }

    fn exec_name(&mut self, n: &str, src: &mut Src) -> Result<(), Flow> {
        self.spend(1)?;
        if let Some(v) = self.lookup(n) {
            return match v {
                V::Proc(_) => self.exec_obj(v, src),
                // An operator stored under another name (`/mo /moveto load def`, as Illustrator's
                // EPS prolog does for every path operator) runs that operator. It pushed the
                // operator's name instead, so such files drew nothing (#955).
                V::Op(op) => self.builtin(&op, src),
                // An executable name (`/y /x cvx def`) runs what that name is bound to.
                V::Name(_) => self.exec_obj(v, src),
                other => {
                    self.push(other);
                    Ok(())
                }
            };
        }
        self.builtin(n, src)
    }

    /// Count `n` operations against the budget, and stop the program when fewer are left. An
    /// operator that makes, reads or copies path elements counts each one as an operation.
    pub(super) fn spend(&mut self, n: usize) -> Result<(), Flow> {
        match self.budget.checked_sub(n) {
            Some(left) => {
                self.budget = left;
                Ok(())
            }
            None => {
                self.b.skip("PostScript (operation limit)");
                Err(Flow::Quit)
            }
        }
    }
}
