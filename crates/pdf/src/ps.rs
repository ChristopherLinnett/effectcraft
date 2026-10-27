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
    /// Fonts by name: `definefont`d dictionaries.
    font_dir: HashMap<String, DictRef>,
    /// Embedded font programs found in the file (name → (program, CFF)).
    embedded: HashMap<String, (Rc<Vec<u8>>, bool)>,
    /// Fonts built for drawing, by base font and encoding.
    font_cache: HashMap<String, Rc<Font>>,
    page: [f64; 4],
    pub unknown: Vec<String>,
}

#[cfg(test)]
mod allocation_tests {
    use super::exec::MAX_STACK;
    use super::*;

    /// Repeated short array/procedure slices spend only their returned slots, not source snapshots.
    #[test]
    fn eps_getinterval_short_slices_charge_only_returned_slots() {
        let mut failures = vec![];
        for setup in ["/a 4096 array def", "/a 4096 array cvx def"] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(setup.as_bytes());
            assert_eq!(it.lookup("a").unwrap().item_count(), 4096);
            it.allocation_budget = 3 * std::mem::size_of::<V>();
            it.run(b"3 { /a load 17 1 getinterval pop } repeat 0 0 10 10 rectfill");
            let left = it.allocation_budget;
            let (nodes, _, skipped) = it.b.finish();
            if left != 0 || nodes.len() != 1 || !skipped.is_empty() {
                failures.push((setup, left, nodes.len(), skipped));
            }
        }
        assert!(failures.is_empty(), "short intervals must fit their exact cumulative budget: {failures:?}");
    }

    /// A one-byte-short interval budget quits through `stopped` without losing earlier slices or art.
    #[test]
    fn eps_getinterval_refuses_only_the_slice_that_exceeds_the_budget() {
        for setup in ["/a 4096 array def", "/a 4096 array cvx def"] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(setup.as_bytes());
            it.run(b"0 0 10 10 rectfill");
            it.allocation_budget = 3 * std::mem::size_of::<V>() - 1;
            it.run(b"{ 3 { /a load 17 1 getinterval } repeat } stopped 20 20 10 10 rectfill");
            assert_eq!(it.st.len(), 2, "{setup}: only two complete slices fit");
            assert!(it.st.iter().all(|v| matches!(v, V::Array(_)) && v.item_count() == 1));
            assert_eq!(it.allocation_budget, std::mem::size_of::<V>() - 1);
            let (nodes, _, skipped) = it.b.finish();
            assert_eq!(nodes.len(), 1, "{setup}: the resource quit must escape stopped and retain prior artwork");
            assert_eq!(skipped, ["PostScript (allocation limit)"]);
        }
    }

    /// Array/procedure intervals clamp empty, oversized and saturating ranges before charging.
    #[test]
    fn eps_getinterval_clamps_compound_bounds_before_charging() {
        for setup in ["/a [ 11 22 33 ] def", "/a { 11 22 33 } def"] {
            for (range, expected) in [
                ("1 0", &[][..]),
                ("3 1", &[][..]),
                ("99 99", &[][..]),
                ("9999999999999999999999999999999999999999 9999999999999999999999999999999999999999", &[][..]),
                ("2 9", &[33.0][..]),
                ("1 9999999999999999999999999999999999999999", &[22.0, 33.0][..]),
                ("0 3", &[11.0, 22.0, 33.0][..]),
            ] {
                let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
                it.run(setup.as_bytes());
                it.allocation_budget = expected.len() * std::mem::size_of::<V>();
                it.run(format!("/a load {range} getinterval").as_bytes());
                assert_eq!(it.st.len(), 1, "{setup}: {range}");
                let V::Array(part) = it.st.last().unwrap() else { panic!("intervals must still return arrays") };
                assert_eq!(part.borrow().iter().map(|v| v.num().unwrap()).collect::<Vec<_>>(), expected);
                assert_eq!(it.allocation_budget, 0);
                let (_, _, skipped) = it.b.finish();
                assert!(skipped.is_empty(), "{setup}: {range}: {skipped:?}");
            }
        }
    }

    /// Interval payloads retain slot order and shallow references to mutable compound values.
    #[test]
    fn eps_getinterval_preserves_order_and_shallow_identity() {
        for setup in ["/a [ 0 [ 11 ] (ab) /tag 9 ] def", "/a [ 0 [ 11 ] (ab) /tag 9 ] cvx def"] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(setup.as_bytes());
            let source = it.lookup("a").unwrap().items().unwrap();
            it.allocation_budget = 4 * std::mem::size_of::<V>();
            it.run(b"/a load 1 4 getinterval /slice exch def /a load 1 get 0 77 put /a load 2 get 0 122 put slice");
            let V::Array(part) = it.st.last().unwrap() else { panic!("intervals must still return arrays") };
            let part = part.borrow();
            assert_eq!(part.len(), 4);
            assert!(matches!((&source[1], &part[0]), (V::Array(a), V::Array(b)) if Rc::ptr_eq(a, b) && b.borrow()[0].num() == Some(77.0)));
            assert!(matches!((&source[2], &part[1]), (V::Str(a), V::Str(b)) if Rc::ptr_eq(a, b) && *b.borrow() == b"zb"));
            assert!(matches!(&part[2], V::Lit(name) if name.as_ref() == "tag"));
            assert_eq!(part[3].num(), Some(9.0));
            assert_eq!(it.allocation_budget, 0);
            let (_, _, skipped) = it.b.finish();
            assert!(skipped.is_empty(), "{setup}: {skipped:?}");
        }
    }

    /// String intervals keep their existing byte-based bounds, exact charge and refusal behavior.
    #[test]
    fn eps_getinterval_preserves_string_budget_and_bounds() {
        for (range, expected, budget, allowed) in [
            ("1 2", &b"bc"[..], 2, true),
            ("1 2", &b"bc"[..], 1, false),
            ("1 0", &b""[..], 0, true),
            ("99 99", &b""[..], 0, true),
            ("9999999999999999999999999999999999999999 9999999999999999999999999999999999999999", &b""[..], 0, true),
            ("2 9999999999999999999999999999999999999999", &b"cd"[..], 2, true),
        ] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(b"/s (abcd) def");
            it.allocation_budget = budget;
            it.run(format!("s {range} getinterval").as_bytes());
            assert_eq!(it.st.len(), usize::from(allowed));
            if allowed {
                assert!(matches!(it.st.last(), Some(V::Str(s)) if *s.borrow() == expected));
                assert_eq!(it.allocation_budget, 0);
            } else {
                assert_eq!(it.allocation_budget, budget);
            }
            let (_, _, skipped) = it.b.finish();
            if allowed {
                assert!(skipped.is_empty(), "{range}: {skipped:?}");
            } else {
                assert_eq!(skipped, ["PostScript (allocation limit)"]);
            }
        }
    }

    /// Exhausted string equality must stop execution before subsequent artwork is drawn.
    #[test]
    fn eps_string_comparison_eq_stops_at_exhausted_budget() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"/s 10 string def");
        it.allocation_budget = 0;
        it.run(b"s s eq pop 0 0 10 10 rectfill");
        let (nodes, _, skipped) = it.b.finish();
        assert!(nodes.is_empty(), "a denied comparison copy must stop execution before rectfill");
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
    }

    /// String inequality must honor the same copy refusal as equality.
    #[test]
    fn eps_string_comparison_ne_stops_at_exhausted_budget() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"/s 10 string def");
        it.allocation_budget = 0;
        it.run(b"s s ne pop 0 0 10 10 rectfill");
        let (nodes, _, skipped) = it.b.finish();
        assert!(nodes.is_empty(), "a denied comparison copy must stop execution before rectfill");
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
    }

    /// Discarding comparison results must not refund their temporary string-copy charges.
    #[test]
    fn eps_string_comparison_repeated_copies_are_cumulative() {
        for op in ["eq", "ne"] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(b"/s 10 string def");
            // Two worst-case UTF-8 conversion bounds fit exactly once, not twice.
            it.allocation_budget = 60;
            it.run(format!("2 {{ s s {op} pop }} repeat 0 0 10 10 rectfill").as_bytes());
            let (nodes, _, skipped) = it.b.finish();
            assert!(nodes.is_empty(), "{op}: discarded comparison strings must not refund the budget");
            assert_eq!(skipped, ["PostScript (allocation limit)"], "{op}");
        }
    }

    /// Preserve lenient cross-type, lossy UTF-8, empty-key and compound-identity comparisons.
    #[test]
    fn eps_string_comparison_preserves_existing_equality_semantics() {
        // These are compatibility controls, including lenient cross-type and lossy-UTF-8
        // comparisons and the existing rule that an empty key is never equal.
        for (operands, expected) in [
            ("(same) (same)", true),
            ("(same) (other)", false),
            ("() ()", false),
            ("() /same", false),
            ("/same ()", false),
            ("<ff> <fe>", true),
            ("<ff> <efbfbd>", true),
            ("<ff> (x)", false),
            ("(same) /same", true),
            ("/same (same)", true),
            ("/same /same", true),
            ("/same /other", false),
            ("(12) 12", true),
            ("12 (12)", true),
            ("(true) true", true),
            ("true (true)", true),
            ("1 1", true),
            ("1 2", false),
            ("true true", true),
            ("true false", false),
            ("1 true", false),
            ("null null", true),
            ("0 array dup", true),
            ("0 array 0 array", false),
            ("0 dict dup", true),
            ("0 dict 0 dict", false),
            ("{} dup", false),
        ] {
            for op in ["eq", "ne"] {
                let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
                it.run(format!("{operands} {op}").as_bytes());
                assert_eq!(it.st.len(), 1, "{operands} {op}");
                assert!(matches!(it.st.last(), Some(V::Bool(b)) if *b == (expected == (op == "eq"))), "{operands} {op}");
                let (_, _, skipped) = it.b.finish();
                assert!(skipped.is_empty(), "{operands} {op}: {skipped:?}");
            }
        }
    }

    /// Each UTF-8 conversion must fit before copying, while short-circuited operands cost nothing.
    #[test]
    fn eps_string_comparison_charges_each_lossy_copy_before_conversion() {
        for op in ["eq", "ne"] {
            for (operands, budget, left, allowed) in [
                ("(0123456789) (0123456789)", 60, 0, true),
                ("(0123456789) (0123456789)", 59, 29, false),
                ("/same (same)", 11, 11, false),
                ("(same) /same", 11, 11, false),
                ("<fffe> <fffe>", 12, 0, true),
                ("<fffe> <fffe>", 11, 5, false),
                ("() ()", 0, 0, true),
                ("() (not copied)", 0, 0, true),
                ("1 1", 0, 0, true),
                ("true false", 0, 0, true),
            ] {
                let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
                it.allocation_budget = budget;
                it.run(format!("{operands} {op}").as_bytes());
                assert_eq!(it.allocation_budget, left, "{operands} {op}, budget {budget}");
                assert_eq!(it.st.len(), usize::from(allowed), "denied comparisons must not push a partial result");
                let (_, _, skipped) = it.b.finish();
                if allowed {
                    assert!(skipped.is_empty(), "{operands} {op}: {skipped:?}");
                } else {
                    assert_eq!(skipped, ["PostScript (allocation limit)"], "{operands} {op}");
                }
            }
        }
    }

    /// A comparison resource quit must escape `stopped` while retaining earlier artwork.
    #[test]
    fn eps_string_comparison_quit_inside_stopped_preserves_prior_artwork() {
        for op in ["eq", "ne"] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(b"0 0 10 10 rectfill /s 10 string def");
            it.allocation_budget = 0;
            it.run(format!("{{ s s {op} pop 20 20 10 10 rectfill }} stopped pop 40 40 10 10 rectfill").as_bytes());
            let (nodes, _, skipped) = it.b.finish();
            assert_eq!(nodes.len(), 1, "{op}: stopped must not swallow a resource quit or discard prior artwork");
            assert_eq!(skipped, ["PostScript (allocation limit)"]);
        }
    }

    /// The executable-string estimate must still admit nested procedures and ASCII85 zero groups.
    #[test]
    fn eps_cvx_accepts_nested_procedures_and_ascii85_zero_expansion() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"({ { 7 } exec } exec <~z~> length) cvx exec");
        assert_eq!(it.st.iter().filter_map(V::num).collect::<Vec<_>>(), [7.0, 4.0]);
        let (_, _, skipped) = it.b.finish();
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// Constructors, intervals and conversions must each refuse an over-budget runtime payload.
    #[test]
    fn eps_runtime_copies_share_the_allocation_budget() {
        // A reduced private counter exercises the real operators without large allocations.
        // Setup allocates before the seam; each body tries to allocate more than 64 bytes.
        let mut missed = vec![];
        for (setup, body) in [
            ("", "10 array"),
            ("", "65 string"),
            ("/a 10 array def", "a 0 10 getinterval"),
            ("/s 65 string def", "s 0 65 getinterval"),
            ("/a 10 array def", "a cvx"),
            ("/p { 1 2 3 4 5 6 7 8 9 10 } def", "/p load cvlit"),
            ("/s (1 2 3 4 5 6 7 8 9 10) def", "s cvx"),
            ("", "[ 1 2 3 4 5 6 7 8 9 10 ]"),
            ("/a 10 array def", "1 2 3 4 5 6 7 8 9 10 a astore"),
            ("/a 10 array def", "a 10 array copy"),
            ("/a 10 array def", "a {} forall"),
            ("/s 65 string def", "s {} forall"),
        ] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(setup.as_bytes());
            it.allocation_budget = 64;
            it.run(format!("{body} clear 0 0 10 10 rectfill").as_bytes());
            let (nodes, _, skipped) = it.b.finish();
            if skipped != ["PostScript (allocation limit)"] || !nodes.is_empty() {
                missed.push(body);
            }
        }
        assert!(missed.is_empty(), "unbudgeted operator allocations: {missed:?}");
    }

    /// Read-side snapshots, matrix values and encodings must share the runtime payload cap.
    #[test]
    fn eps_runtime_read_snapshots_share_the_allocation_budget() {
        let mut missed = vec![];
        for (setup, body) in [
            ("/a 10 array def", "a 0 setdash"),
            ("/a 10 array def", "a rectfill"),
            ("/a 10 array def", "a 10 array def"),
            ("/a 10 array def /b 10 array def", "a b copy"),
            ("/s 65 string def", "s show"),
            ("/s 65 string def", "s 0 string cvs"),
            ("", "StandardEncoding"),
            ("", "matrix matrix"),
        ] {
            let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
            it.run(setup.as_bytes());
            it.allocation_budget = 64;
            it.run(format!("{body} clear").as_bytes());
            let (_, _, skipped) = it.b.finish();
            if skipped != ["PostScript (allocation limit)"] {
                missed.push(body);
            }
        }
        assert!(missed.is_empty(), "unbudgeted payloads: {missed:?}");
    }

    /// Numeric `copy` must charge all new operand slots before extending the stack.
    #[test]
    fn eps_numeric_copy_charges_new_operand_slots() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"1 2 3 4");
        it.allocation_budget = 0;
        it.run(b"4 copy 0 0 10 10 rectfill");
        assert_eq!(it.st.len(), 4, "refuse before copying any operands");
        let (nodes, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
        assert!(nodes.is_empty());
    }

    /// Small built-in strings must accumulate charges just like explicit string constructors.
    #[test]
    fn eps_small_runtime_strings_share_the_allocation_budget() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.allocation_budget = 7;
        it.run(b"version product 0 0 10 10 rectfill");
        let (nodes, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
        assert!(nodes.is_empty());
    }

    /// Exact stack limits must preserve operand order, shared arrays and consume-first `astore`.
    #[test]
    fn eps_stack_boundaries_preserve_order_and_compound_identity() {
        let nums = |it: &Interp| it.st.iter().map(|v| v.num().unwrap()).collect::<Vec<_>>();
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"1 2 3 2 copy");
        assert_eq!(nums(&it), [1.0, 2.0, 3.0, 2.0, 3.0]);
        it.run(b"clear [ 4 5 6 ] 3 array copy aload pop");
        assert_eq!(nums(&it), [4.0, 5.0, 6.0]);
        it.run(b"clear 7 8 9 3 array astore aload pop");
        assert_eq!(nums(&it), [7.0, 8.0, 9.0]);
        it.run(b"clear /a [ 10 11 ] def a 1 copy 0 99 put clear a aload pop pop");
        assert_eq!(nums(&it), [99.0]);

        // Numeric copy and aload admit the exact final depth, including aload's array.
        it.st = vec![V::Num(1.0); MAX_STACK / 2];
        it.run(b"50000 copy");
        assert_eq!(it.st.len(), MAX_STACK);
        it.run(b"clear 99999 array aload");
        assert_eq!(it.st.len(), MAX_STACK);
        assert!(matches!(it.st.last(), Some(V::Array(_))));
        // astore consumes before returning its array; it needs no extra stack room.
        it.run(b"astore");
        assert_eq!(it.st.len(), 1);
        assert_eq!(it.st[0].item_count(), MAX_STACK - 1);
        let (_, _, skipped) = it.b.finish();
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// Array slots and string bytes must consume one counter without refunds on overwrite or drop.
    #[test]
    fn eps_mixed_payloads_exhaust_one_budget_without_refunds() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        let slots = 2 * std::mem::size_of::<V>();
        it.allocation_budget = slots + 3;
        it.run(b"/a 2 array def 3 string pop /a null def");
        assert_eq!(it.allocation_budget, 0);
        it.run(b"0 string pop 1 string");
        assert_eq!(it.allocation_budget, 0);
        assert!(it.st.is_empty(), "refused allocation must not push a partial result");
        let (_, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
    }

    /// `aload` must check its whole result, including the array, before cloning or charging slots.
    #[test]
    fn eps_aload_refuses_before_cloning_or_pushing_items() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"42 /a 100000 array def");
        let left = it.allocation_budget;
        it.run(b"a aload");
        assert_eq!(it.st.len(), 1);
        assert_eq!(it.st[0].num(), Some(42.0));
        assert_eq!(it.allocation_budget, left);
        let (_, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (operand stack limit)"]);
    }

    /// An overflowing numeric `copy` must leave both stack contents and allocation budget intact.
    #[test]
    fn eps_copy_refuses_before_extending_the_stack() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.st = vec![V::Num(1.0); MAX_STACK / 2 + 1];
        let left = it.allocation_budget;
        it.run(b"50000 copy");
        assert_eq!(it.st.len(), MAX_STACK / 2 + 1);
        assert_eq!(it.allocation_budget, left);
        let (_, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (operand stack limit)"]);
    }

    /// Repeated discarded payloads must exhaust the cumulative cap despite bounded live storage.
    #[test]
    fn eps_runtime_payload_allocations_are_cumulative() {
        // Only one 1 MB string is live at a time. The reduced counter exercises repeated
        // allocation even when each object is discarded before the next one is created.
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.allocation_budget = 2_500_000;
        it.run(b"3 { 1000000 string pop } repeat 0 0 10 10 rectfill");
        let (nodes, _, skipped) = it.b.finish();
        assert_eq!(skipped, ["PostScript (allocation limit)"]);
        assert!(nodes.is_empty(), "execution must stop, not merely discard an allocation");
    }

    /// Length, element and matrix reads must still work when no payload-copy budget remains.
    #[test]
    fn eps_reads_do_not_copy_entire_arrays() {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(b"/a [ 1 2 3 ] def /m [ 2 0 0 3 0 0 ] def");
        it.allocation_budget = 0;
        it.run(b"a length a 1 get m setmatrix 1 1 transform");
        assert_eq!(it.st.iter().filter_map(V::num).collect::<Vec<_>>(), [3.0, 2.0, 2.0, 3.0]);
        let (_, _, skipped) = it.b.finish();
        assert!(skipped.is_empty(), "{skipped:?}");
    }
}
