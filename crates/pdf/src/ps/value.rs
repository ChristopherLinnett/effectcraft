use super::*;

pub(crate) type DictRef = Rc<RefCell<HashMap<String, V>>>;

#[derive(Clone)]
pub(crate) enum V {
    Num(f64),
    Bool(bool),
    /// An executable name.
    Name(Rc<str>),
    /// A literal name (`/name`).
    Lit(Rc<str>),
    /// A built-in operator, as `load` returns it for a name no dictionary defines.
    Op(Rc<str>),
    Str(Rc<RefCell<Vec<u8>>>),
    Proc(Rc<Vec<V>>),
    Array(Rc<RefCell<Vec<V>>>),
    Dict(DictRef),
    Mark,
    Null,
    /// `currentfile`.
    File,
}

impl V {
    pub(super) fn num(&self) -> Option<f64> {
        match self {
            V::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub(super) fn key(&self) -> String {
        match self {
            V::Name(n) | V::Lit(n) | V::Op(n) => n.to_string(),
            V::Str(s) => String::from_utf8_lossy(&s.borrow()).into_owned(),
            V::Num(n) => format!("{n}"),
            V::Bool(b) => format!("{b}"),
            _ => String::new(),
        }
    }
    /// Return the array/procedure length without cloning its payload; other values have no items.
    pub(super) fn item_count(&self) -> usize {
        match self {
            V::Array(a) => a.borrow().len(),
            V::Proc(p) => p.len(),
            _ => 0,
        }
    }

    pub(super) fn items(&self) -> Option<Vec<V>> {
        match self {
            V::Array(a) => Some(a.borrow().clone()),
            V::Proc(p) => Some(p.as_ref().clone()),
            _ => None,
        }
    }
}

pub(super) enum Flow {
    Exit,
    Stop,
    Quit,
}

#[derive(Clone)]
pub(super) struct GState {
    pub(crate) ctm: Affine,
    pub(crate) color: [f64; 3],
    pub(crate) lw: f64,
    pub(crate) cap: Cap,
    pub(crate) join: Join,
    pub(crate) miter: f64,
    pub(crate) dash: Option<(Vec<f64>, f64)>,
    pub(crate) path: BezPath,
    pub(crate) cur: Option<Point>,
    pub(crate) start: Option<Point>,
    pub(crate) depth: usize,
    /// The current font dictionary.
    pub(crate) font: Option<DictRef>,
}

pub(super) fn new_dict() -> DictRef {
    Rc::new(RefCell::new(HashMap::new()))
}

pub(super) fn array(v: Vec<V>) -> V {
    V::Array(Rc::new(RefCell::new(v)))
}

/// Borrow an array/procedure and read exactly six numeric entries, ignoring nonnumeric entries.
pub(super) fn matrix_of(v: &V) -> Option<Affine> {
    let read = |items: &[V]| {
        let mut nums = items.iter().filter_map(V::num);
        let mut a = [0.0; 6];
        for n in &mut a {
            *n = nums.next()?;
        }
        nums.next().is_none().then(|| Affine::new(a))
    };
    match v {
        V::Array(a) => read(&a.borrow()),
        V::Proc(p) => read(p),
        _ => None,
    }
}

pub(super) fn matrix_val(m: Affine) -> V {
    array(m.as_coeffs().iter().map(|c| V::Num(*c)).collect())
}
