//! Encapsulated PostScript: a small PostScript interpreter (PostScript Language Reference, 3rd
//! edition, and the EPSF 3.0 specification) covering what exported vector EPS files use: the
//! operand and dictionary stacks, procedures and `def`, arithmetic, control flow, arrays and
//! dictionaries, the graphics state, path construction (including arcs) and painting, clipping
//! and device colours, and text (`findfont` / `selectfont`, `show` and its variants, `charpath`,
//! `stringwidth`, re-encoded fonts) with embedded Type 1 and CFF programs or the bundled
//! stand-in fonts. Images are skipped (their inline data is stepped over), as is
//! Level 3 smooth shading (`shfill`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use effectcraft_svg::{Affine, BezPath, Cap, FillRule, Join, Paint, Stroke};
use kurbo::{PathEl, Point};

use crate::build::{Builder, MAX_PATH};
use crate::font::Font;
use crate::object::{ascii85, is_white, parse_num};

mod builtin;
mod eps;
mod exec;
mod paths;
mod scanner;
mod text;
mod value;

pub use eps::eps_parts;
use value::{DictRef, GState, V};

/// A drawable font's cache key: its base font name and the glyph names it draws with.
type FontKey = (Rc<str>, Glyphs);

/// The glyph names a drawable font gives codes 0 to 255, shared with the values that hold them:
/// the first 256 entries of its encoding ([`Font::standalone`] reads no more), the one glyph
/// `glyphshow` draws as code 65, or its program's own.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Glyphs {
    Encoding(Vec<Option<Rc<str>>>),
    One(Rc<str>),
    Builtin,
}

pub(crate) struct Interp {
    pub b: Builder,
    st: Vec<V>,
    dicts: Vec<DictRef>,
    gs: GState,
    saved: Vec<GState>,
    budget: usize,
    /// Cumulative runtime array/procedure payload bytes and string bytes; never refunded.
    allocation_budget: usize,
    /// Nested procedure calls (recursion is cut off at [`MAX_CALL_DEPTH`]).
    calls: usize,
    /// Fonts by name (Font Directory): `definefont`d and `findfont`-made dictionaries.
    font_dir: DictRef,
    /// Embedded font programs found in the file (name → (program, CFF)).
    embedded: HashMap<String, (Rc<Vec<u8>>, bool)>,
    /// Fonts built for drawing, by base font and the glyph names of their encoding, which the
    /// key shares with the dictionaries that hold them.
    font_cache: HashMap<FontKey, Rc<Font>>,
    page: [f64; 4],
    pub unknown: Vec<String>,
}

#[cfg(test)]
mod allocation_tests;
