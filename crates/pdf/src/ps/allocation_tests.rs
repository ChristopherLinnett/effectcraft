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
        // The interval's four slots, and the new entry `slice` the dictionary keeps.
        it.allocation_budget = 4 * std::mem::size_of::<V>() + "slice".len() + std::mem::size_of::<(String, V)>();
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

/// Array slots, string bytes and dictionary keys must consume one counter without refunds on
/// overwrite or drop; overwriting a key's value costs nothing more.
#[test]
fn eps_mixed_payloads_exhaust_one_budget_without_refunds() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    let slots = 2 * std::mem::size_of::<V>();
    // Two slots, three string bytes, and the entry `a` once: its second `def` overwrites.
    it.allocation_budget = slots + 3 + "a".len() + std::mem::size_of::<(String, V)>();
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
fn eps_reads_work_with_no_allocation_budget_left() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/a [ 1 2 3 ] def /m [ 2 0 0 3 0 0 ] def");
    it.allocation_budget = 0;
    it.run(b"a length a 1 get m setmatrix 1 1 transform");
    assert_eq!(it.st.iter().filter_map(V::num).collect::<Vec<_>>(), [3.0, 2.0, 2.0, 3.0]);
    let (_, _, skipped) = it.b.finish();
    assert!(skipped.is_empty(), "{skipped:?}");
}

/// A fresh interpreter has the 256 MiB runtime allocation budget the README documents: strings
/// totalling exactly 256 MiB fit, one byte more quits with the allocation-limit note and draws
/// nothing after.
#[test]
fn eps_interpreters_start_with_a_256_mib_allocation_budget() {
    // 26 strings of 10 000 000 bytes, then the rest of 268 435 456 (or one byte past it).
    let rest = (256 << 20) - 26 * 10_000_000;
    for (last, fits) in [(rest, true), (rest + 1, false)] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(format!("26 {{ 10000000 string pop }} repeat {last} string pop 0 0 10 10 rectfill").as_bytes());
        let (nodes, _, skipped) = it.b.finish();
        if fits {
            assert!(skipped.is_empty() && nodes.len() == 1, "256 MiB fits: {skipped:?}");
        } else {
            assert!(nodes.is_empty() && skipped == ["PostScript (allocation limit)"], "one byte more doesn't: {skipped:?}");
        }
    }
}

/// Text bytes a value holds: its string or name, and those of the arrays and dictionaries
/// (keys included) inside it.
fn held_text(v: &V) -> usize {
    match v {
        V::Str(s) => s.borrow().len(),
        V::Name(n) | V::Lit(n) | V::Op(n) => n.len(),
        V::Array(a) => a.borrow().iter().map(held_text).sum(),
        V::Proc(p) => p.iter().map(held_text).sum(),
        V::Dict(d) => d.borrow().iter().map(|(k, v)| k.len() + held_text(v)).sum(),
        _ => 0,
    }
}

/// `load` of an undefined name keeps the name in the operator it makes, `cvs` and `cvn` keep the
/// text they convert to, `forall` over a dictionary hands out a fresh copy of each key, and a
/// dictionary keeps its own copy of every key added to it (by `def`, `store`, `put`, `>>`, a
/// font's `scalefont` copy, or `findfont` and `definefont` naming a new font), so each charges
/// that text's full length first: made, converted, handed out or added over and over and kept in
/// an array, it never holds more than the budget.
#[test]
fn eps_conversions_charge_the_text_they_keep() {
    let long = "a".repeat(10_000);
    let budget = 100_000;
    let mut over = vec![];
    let dict = format!("/d 1 dict def d /{long} 0 put");
    let string = "/s 10000 string def /i 0 def";
    // A new font name each time, from one string: its first byte counts up.
    let next = "s 0 i put /i i 1 add def";
    let font = "/s 10000 string def /f 1 dict def f s 0 put";
    for (what, setup, body) in [
        ("load of an undefined name", "", format!("/{long} load")),
        ("load of an undefined string", "", format!("({long}) load")),
        ("cvs of a loaded operator", "", format!("/{long} load 0 string cvs")),
        ("cvs of a name", "", format!("/{long} 0 string cvs")),
        ("cvs of a string", "", format!("({long}) 0 string cvs")),
        ("cvn of a loaded operator", "", format!("/{long} load cvn")),
        ("cvn of a string", "", format!("({long}) cvn")),
        ("forall over a dictionary's key", dict.as_str(), "d { pop } forall".to_string()),
        ("put of a string key into new dictionaries", string, "1 dict dup s 0 put".to_string()),
        ("def of a string key into new dictionaries", string, "1 dict begin s 0 def currentdict end".to_string()),
        ("def of a name key into new dictionaries", "", format!("1 dict begin /{long} 0 def currentdict end")),
        ("store of a string key into new dictionaries", string, "1 dict begin s 0 store currentdict end".to_string()),
        ("<< >> with a string key", string, "<< s 0 >>".to_string()),
        ("scalefont copies of a font with a long key", font, "f 10 scalefont".to_string()),
        ("findfont of new font names", string, format!("{next} s findfont")),
        ("definefont of new font names", string, format!("{next} s 1 dict definefont")),
    ] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(setup.as_bytes());
        it.allocation_budget = budget;
        it.run(format!("[ 50 {{ {body} }} repeat ] 0 0 10 10 rectfill").as_bytes());
        let held: usize = it.st.iter().map(held_text).sum();
        let (nodes, _, skipped) = it.b.finish();
        if held > budget || !nodes.is_empty() || skipped != ["PostScript (allocation limit)"] {
            over.push(format!("{what}: {held} bytes kept, {skipped:?}"));
        }
    }
    assert!(over.is_empty(), "conversions past a {budget}-byte budget: {over:#?}");
}

/// A dictionary charges an entry once, when it is added: defining, storing or putting a new value
/// under the same key again costs nothing more.
#[test]
fn eps_dictionary_keys_are_charged_once() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.allocation_budget = "key".len() + std::mem::size_of::<(String, V)>();
    it.run(b"/key 0 def 100 { /key 1 def /key 2 store currentdict /key 3 put } repeat 0 0 10 10 rectfill");
    assert_eq!(it.allocation_budget, 0, "the entry, charged once");
    let (nodes, _, skipped) = it.b.finish();
    assert!(skipped.is_empty() && nodes.len() == 1, "overwrites are free: {skipped:?}");
}

/// `forall` over a dictionary snapshots its entries before running the procedure, each as a key-
/// value pair whose key is a literal name of its own: the whole snapshot is charged before it is
/// taken, however early the procedure exits. Five 10 000-byte keys need their 50 000 bytes and the
/// snapshot's five pairs of slots.
#[test]
fn eps_forall_charges_the_dictionary_snapshot_first() {
    let a = "a".repeat(9_999);
    let setup = format!("/d 5 dict def d /{a}0 0 put d /{a}1 0 put d /{a}2 0 put d /{a}3 0 put d /{a}4 0 put");
    let slots = 5 * 2 * std::mem::size_of::<V>();
    for (budget, fits) in [(50_000 + slots, true), (50_000 + slots - 1, false)] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(setup.as_bytes());
        it.allocation_budget = budget;
        it.run(b"d { pop pop exit } forall 0 0 10 10 rectfill");
        let left = it.allocation_budget;
        let (nodes, _, skipped) = it.b.finish();
        if fits {
            assert!(left == 0 && nodes.len() == 1 && skipped.is_empty(), "{budget}: {left} left, {skipped:?}");
        } else {
            assert!(nodes.is_empty() && skipped == ["PostScript (allocation limit)"], "{budget}: {skipped:?}");
        }
    }
}

/// A new font keeps its name, six entries and two six-slot matrices (FontMatrix and its own), plus
/// a Font Directory entry; a `scalefont` copy keeps an entry for every key and a matrix of its own;
/// a font `definefont` completes keeps the program's name, two entries, a matrix and a Font
/// Directory entry: all charged, entries by their key text and a fixed slot, matrices by their
/// slots.
#[test]
fn eps_fonts_charge_their_keys_and_matrices() {
    let slot = std::mem::size_of::<V>();
    let entry = std::mem::size_of::<(String, V)>();
    let keys: usize = ["FontName", "FontType", "FontMatrix", "FID", "__base", "__m"].iter().map(|k| k.len()).sum();
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    let spent = |it: &mut Interp, ps: &[u8]| {
        let before = it.allocation_budget;
        it.run(ps);
        before - it.allocation_budget
    };
    // The name, once for the font and once as its Font Directory key; the six dictionary entries
    // and the Font Directory entry.
    assert_eq!(spent(&mut it, b"/F findfont pop"), 2 + keys + 12 * slot + 7 * entry, "findfont of a new font");
    assert_eq!(spent(&mut it, b"/F findfont pop"), 0, "findfont of a known font");
    assert_eq!(spent(&mut it, b"/F findfont 10 scalefont pop"), keys + 6 * entry + 6 * slot, "scalefont");
    assert_eq!(spent(&mut it, b"/G 1 dict definefont pop"), 2 + "__base".len() + "__m".len() + 6 * slot + 3 * entry, "definefont");
}

/// `glyphshow` draws through a cached font that names just that glyph: the first use of a glyph
/// charges what that font keeps, and drawing it again costs nothing, however many times.
#[test]
fn eps_glyphshow_charges_a_new_glyph_once() {
    use std::mem::size_of;
    let entry = |glyph: &str| size_of::<Font>() + 256 * size_of::<Option<String>>() + "F".len() + glyph.len() + 255 * ".notdef".len();
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/F findfont 10 scalefont setfont 0 0 moveto");
    let mut spent = |ps: &[u8]| {
        let before = it.allocation_budget;
        it.run(ps);
        before - it.allocation_budget
    };
    assert_eq!(spent(b"/A glyphshow"), entry("A"), "a new glyph");
    assert_eq!(spent(b"1000 { /A glyphshow } repeat"), 0, "the same glyph again");
    assert_eq!(spent(b"/Bee glyphshow"), entry("Bee"), "another glyph");
}

/// `gsave` and `save` keep a copy of the graphics state, dash array included: each copy's dash
/// is charged, and at most `MAX_SAVE_DEPTH` copies are kept at once.
#[test]
fn eps_gsave_charges_the_dash_it_copies() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/a 1000 array def 0 1 999 { a exch 1 put } for a 0 setdash");
    let dash = 1000 * std::mem::size_of::<f64>();
    for op in ["gsave", "save pop"] {
        let before = it.allocation_budget;
        it.run(op.as_bytes());
        assert_eq!(before - it.allocation_budget, dash, "{op}");
    }
    it.allocation_budget = 3 * dash - 1;
    it.run(b"3 { gsave } repeat 0 0 10 10 rectfill");
    let (nodes, _, skipped) = it.b.finish();
    assert!(nodes.is_empty() && skipped == ["PostScript (allocation limit)"], "three copies past the budget: {skipped:?}");
}

/// Graphics-state copies stop at `MAX_SAVE_DEPTH`: a runaway `gsave` loop ends there, with a note,
/// instead of growing the state stack until the operation limit.
#[test]
fn eps_gsave_stops_at_the_depth_limit() {
    use super::exec::MAX_SAVE_DEPTH;
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"0 0 10 10 rectfill { { gsave } loop } loop");
    assert_eq!(it.saved.len(), MAX_SAVE_DEPTH);
    let (nodes, _, skipped) = it.b.finish();
    assert!(nodes.len() == 1 && skipped == ["PostScript (graphics state depth limit)"], "{skipped:?}");
}

/// A font's encoding is read in place to find its cached font, so showing text with a font
/// already built costs nothing more; building it charges what it keeps first: the font, its
/// 256-name glyph table, its base name and the first 256 encoding entries it is cached by.
#[test]
fn eps_text_charges_a_new_fonts_encoding_names_once() {
    use std::mem::size_of;
    let entry = |name: &str, slots: usize| {
        size_of::<Font>() + 256 * size_of::<Option<String>>() + "Helvetica".len() + slots * size_of::<Option<Rc<str>>>() + 256 * name.len()
    };
    let font = |name: &str, size: usize| {
        format!(
            "/n /{name} def /e {size} array def 0 1 {} {{ e exch n put }} for /f 3 dict def f /FontName /Helvetica put f /Encoding e put f setfont 0 0 moveto",
            size - 1
        )
    };
    let long = "n".repeat(10_000);
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(font(&long, 256).as_bytes());
    it.allocation_budget = 100_000;
    it.run(b"(A) show 0 0 10 10 rectfill");
    let (nodes, _, skipped) = it.b.finish();
    assert!(nodes.is_empty() && skipped == ["PostScript (allocation limit)"], "256 copies of a 10 000-byte name: {skipped:?}");

    // An encoding longer than 256 entries is cached by its first 256: what a font reads.
    for size in [256, 1_000_000] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(font("A", size).as_bytes());
        let before = it.allocation_budget;
        it.run(b"(A) show");
        assert_eq!(before - it.allocation_budget, 1 + entry("A", 256), "{size} entries: the shown string and the new font");
        let before = it.allocation_budget;
        it.run(b"(A) show");
        assert_eq!(before - it.allocation_budget, 1, "{size} entries: the second show charges only its string");
    }
}

/// Hashing or comparing a string used as a dictionary key, a lookup name, or a font name reads
/// every byte, so each such operation charges the string's worst-case scan: a loop over one big
/// string key does bounded work. A name, shared in place, costs nothing.
#[test]
fn eps_string_key_scans_are_charged() {
    let scan = 1000 * 3; // The worst-case lossy UTF-8 length a 1 000-byte string could scan to.
    for (what, setup, op) in [
        ("known", "/d 1 dict def /s 1000 string def d s 0 put", "d s known pop"),
        ("get", "/d 1 dict def /s 1000 string def d s 0 put", "d s get pop"),
        ("put overwrite", "/d 1 dict def /s 1000 string def d s 0 put", "d s 1 put"),
        ("where", "/s 1000 string def s 0 def", "s where pop"),
        ("load", "/s 1000 string def s 0 def", "s load pop"),
        ("findfont hit", "/s 1000 string def s findfont pop", "s findfont pop"),
    ] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(setup.as_bytes());
        let before = it.allocation_budget;
        it.run(op.as_bytes());
        assert_eq!(before - it.allocation_budget, scan, "{what} charges the key scan");
    }
    // The same operations on a name key read it in place and keep nothing, so they cost nothing.
    for (what, setup, op) in [
        ("known", "/d 1 dict def /k 0 def", "d /k known pop"),
        ("get", "/d 1 dict def d /k 0 put", "d /k get pop"),
        ("put overwrite", "/d 1 dict def d /k 0 put", "d /k 1 put"),
        ("where", "/k 0 def", "/k where pop"),
        ("load", "/k 0 def", "/k load pop"),
        ("findfont hit", "/k findfont pop", "/k findfont pop"),
    ] {
        let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
        it.run(setup.as_bytes());
        let before = it.allocation_budget;
        it.run(op.as_bytes());
        assert_eq!(before - it.allocation_budget, 0, "{what} on a name copies nothing");
    }
}

/// `glyphshow` with no current font drops its operand and copies nothing. With a font set, a
/// string glyph name is copied for the cache key each call, and a string font name is copied on
/// every show, so a loop over a huge one stops at the allocation limit rather than copying without
/// bound.
#[test]
fn eps_glyphshow_and_string_font_names_are_bounded() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/s 1000 string def");
    let before = it.allocation_budget;
    it.run(b"100000 { s glyphshow } repeat");
    assert_eq!(before - it.allocation_budget, 0, "glyphshow with no font copies nothing");

    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/F findfont 10 scalefont setfont 0 0 moveto /s 10000000 string def");
    it.run(b"{ { s glyphshow } loop } loop");
    let (_, _, skipped) = it.b.finish();
    assert_eq!(skipped, ["PostScript (allocation limit)"], "a huge string glyph name is bounded");

    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/s 10000000 string def << /FontName s /FontType 1 >> setfont 0 0 moveto");
    it.run(b"{ { (A) show } loop } loop");
    let (_, _, skipped) = it.b.finish();
    assert_eq!(skipped, ["PostScript (allocation limit)"], "a huge string font name is bounded");
}

/// An unknown operator is noted for the report by the start of its name only, however long the
/// name the file made.
#[test]
fn eps_unknown_operators_keep_a_short_name() {
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(format!("({}) cvn cvx exec", "x".repeat(10_000)).as_bytes());
    assert!(!it.unknown.is_empty() && it.unknown.iter().all(|u| u.chars().count() <= 64), "{:?}", it.unknown.iter().map(String::len).collect::<Vec<_>>());
}

/// A conversion charges the exact length of the key text it copies, and a new dictionary entry
/// that length and a fixed entry slot: a number's formatted length, a UTF-8 string's own length, a
/// string that isn't UTF-8 by its replacement. Overwriting an existing key keeps nothing new.
#[test]
fn eps_keys_and_conversions_charge_exact_lengths() {
    let entry = std::mem::size_of::<(String, V)>();
    let mut it = Interp::new([0.0, 0.0, 100.0, 100.0]);
    it.run(b"/d 3 dict def");
    for (ps, bytes) in [
        ("d 12345 0 put", 5 + entry),
        ("d (abc) 0 put", 3 + entry),
        ("d <ff> 0 put", "\u{fffd}".len() + entry),
        ("d 12345 1 put", 0),
        ("(abc) cvn pop", 3),
        ("12345 0 string cvs pop", 5),
        ("true 0 string cvs pop", 4),
    ] {
        let before = it.allocation_budget;
        it.run(ps.as_bytes());
        assert_eq!(before - it.allocation_budget, bytes, "{ps}");
    }
}
