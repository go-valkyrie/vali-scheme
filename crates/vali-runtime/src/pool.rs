//! The pool: where a module's constants live (ADR 0020).
//!
//! A constant is built when its module loads, into frozen pages of its own:
//! outside the heap's lists and its accounting, and held by the isolate for
//! its life. Each object has exactly the layout the strategy gives the same
//! object in the heap, so reads, type tests and list walks need no check.
//! Nothing is counted: the strategy makes a headered object's count sticky
//! (`References::freeze`), and a side-counted pair's page presets its bytes.
//! Pairs, which have no header under some strategies, are frozen by their
//! page: the page header's first word is nonzero. A headered object (string,
//! vector, bytevector) is frozen by `data::IMMUTABLE` in its header. Writes
//! check; reads do not.
//!
//! A pool object points only to other pool objects, with one exception:
//! symbols and keywords, which are the isolate's own, interned as ever and
//! never freed while it lives.
//!
//! The pages come from the isolate's heap, which charges them to its limit
//! and frees them when it is dropped. One pool serves every module the
//! isolate loads, so it takes a page or two of each kind however many
//! modules there are.

use vali_abi::{Header, TypeCode, data};
use vali_bytecode::Constant;
use vali_num::{Integer, Number};

use crate::cx::{Cx, put};
use crate::handle::Error;
use crate::page::{LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES};
use crate::strategy::{Space, Strategy};
use crate::value::Value;

/// The part of a frozen page not yet used: from `cursor` to `end`.
#[derive(Debug, Default, Clone, Copy)]
struct Region {
    cursor: usize,
    end: usize,
}

/// Where the next constant goes: the end of the pair pages and the end of
/// the general ones.
#[derive(Debug, Default)]
pub(crate) struct Pool {
    pairs: Region,
    general: Region,
    /// Whether constants are being built in the heap, as counted objects
    /// frozen like the pool's, for a module the heap frees
    /// (`Cx::load_transient`): then they are held by the module's object and
    /// freed with it, unless something else refers to them.
    in_heap: bool,
}

impl<S: Strategy> Cx<S> {
    /// Whether the pair `v` is a constant of a pool: on a frozen page, the
    /// first word of its page header (`PAGE_HEADER_BYTES`) not zero. Every
    /// write to a pair that might be a constant checks it first.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair.
    #[inline(always)]
    pub(crate) unsafe fn pair_is_frozen(v: Value) -> bool {
        let page = v.address() as usize & !(PAGE_BYTES - 1);
        // SAFETY: every page that holds pairs starts with a header word,
        // and a live pair is on one.
        unsafe { *(page as *const usize) != 0 }
    }

    /// An error saying `name` cannot change the immutable pair `v`.
    pub(crate) fn immutable_pair(&self, name: &str, v: Value) -> Error {
        Error::scheme(format!("{name}: the pair {} is immutable", self.written(v)))
    }

    /// Memory for an object of `words` words in the pool.
    fn pool_words(&mut self, pair: bool, words: usize) -> Result<usize, OutOfMemory> {
        let bytes = words.checked_mul(8).ok_or(OutOfMemory)?;
        if bytes > LARGE_OBJECT_BYTES {
            return Ok(S::pool_large(self.heap_mut(), bytes)?.as_ptr() as usize);
        }
        let region = if pair {
            self.pool.pairs
        } else {
            self.pool.general
        };
        let region = if region.end - region.cursor < bytes {
            let (cursor, end) = if pair {
                S::pool_pair_page(self.heap_mut(), words)?
            } else {
                S::pool_page(self.heap_mut())?
            };
            Region { cursor, end }
        } else {
            region
        };
        let object = region.cursor;
        let region = Region {
            cursor: object + bytes,
            ..region
        };
        if pair {
            self.pool.pairs = region;
        } else {
            self.pool.general = region;
        }
        Ok(object)
    }

    /// A new object of type `t` in the pool, its header written and its
    /// strategy's half frozen, the fields uninitialised.
    fn pool_object(&mut self, t: TypeCode, fields: usize, data: u32) -> Result<Value, OutOfMemory> {
        if self.pool.in_heap {
            let space = if t == TypeCode::Pair {
                Space::Literal
            } else {
                Space::General
            };
            let space = if t == TypeCode::Flonum {
                Space::Flonum
            } else {
                space
            };
            return self.new_object_in(t, fields, data, space);
        }
        let words = Self::LAYOUT.words(t, fields);
        let address = self.pool_words(t == TypeCode::Pair, words)?;
        // SAFETY: the memory was just taken for an object of type `t`.
        let v = unsafe { Value::from_address(t.tag(), address as u64) };
        if let Some(offset) = Self::LAYOUT.header(t) {
            let header = Header::new(t, data).expect("runtime data fits in 24 bits");
            // SAFETY: the object has room for its header.
            unsafe { put(v, offset, header.0) };
        }
        S::freeze(v);
        Ok(v)
    }

    /// Stores the value `x` in a field of `v`, an object just built: counted
    /// if the constants are built in the heap.
    ///
    /// # Safety
    ///
    /// `v` must be an object just built with a value field at `offset`.
    unsafe fn pool_store(&mut self, v: Value, offset: i32, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe {
            if self.pool.in_heap {
                self.init_field(v, offset, x);
            } else {
                put(v, offset, x.raw());
            }
        }
    }

    /// `constant` as a value built in the heap, as counted objects that are
    /// frozen as the pool's are, for a module the heap frees. Each object is
    /// held only by the objects that refer to it: the caller holds the
    /// result.
    pub(crate) fn heap_constant(&mut self, constant: &Constant) -> Result<Value, Error> {
        // Only a counting strategy frees what it builds here.
        debug_assert!(S::COUNTS, "heap constants are for modules a count frees");
        self.pool.in_heap = true;
        let result = self.pool_constant(constant);
        self.pool.in_heap = false;
        result
    }

    /// A pool bytevector holding `bytes`.
    fn pool_bytevector(&mut self, bytes: &[u8], flags: u32) -> Result<Value, OutOfMemory> {
        let v = self.pool_object(TypeCode::Bytevector, 1 + bytes.len().div_ceil(8), flags)?;
        let length = Self::LAYOUT.length(TypeCode::Bytevector);
        // SAFETY: the bytevector has a length word and room for every byte,
        // and nothing can see it yet.
        unsafe {
            put(v, length, bytes.len() as u64);
            let start = v.raw().wrapping_add_signed(i64::from(length) + 8) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), start, bytes.len());
        }
        Ok(v)
    }

    fn pool_flonum(&mut self, x: f64) -> Result<Value, OutOfMemory> {
        let v = self.pool_object(TypeCode::Flonum, 1, 0)?;
        // SAFETY: a flonum has one field.
        unsafe { put(v, Self::LAYOUT.flonum(), x.to_bits()) };
        Ok(v)
    }

    fn pool_integer(&mut self, n: &Integer) -> Result<Value, OutOfMemory> {
        if let Some(v) = n.to_i64().and_then(Value::fixnum) {
            return Ok(v);
        }
        let (negative, limbs) = n.limbs();
        let v = self.pool_object(TypeCode::Bignum, 1 + limbs.len(), u32::from(negative))?;
        // SAFETY: the bignum has room for its count and limbs.
        unsafe {
            put(v, Self::LAYOUT.length(TypeCode::Bignum), limbs.len() as u64);
            for (i, &limb) in limbs.iter().enumerate() {
                put(v, Self::LAYOUT.field(TypeCode::Bignum, 1 + i), limb);
            }
        }
        Ok(v)
    }

    fn pool_number(&mut self, n: &Number) -> Result<Value, OutOfMemory> {
        match n {
            Number::Integer(i) => self.pool_integer(i),
            Number::Real(x) => self.pool_flonum(*x),
            Number::Rational(r) => {
                let numerator = self.pool_integer(&r.numerator())?;
                let denominator = self.pool_integer(&r.denominator())?;
                self.pool_pair_of(TypeCode::Ratnum, numerator, denominator)
            }
            Number::Complex(c) if c.is_exact() => {
                let re = self.pool_number(&c.real_part())?;
                let im = self.pool_number(&c.imag_part())?;
                self.pool_pair_of(TypeCode::ExactComplex, re, im)
            }
            Number::Complex(c) => {
                let (Number::Real(re), Number::Real(im)) = (c.real_part(), c.imag_part()) else {
                    unreachable!("an inexact complex number has inexact parts")
                };
                let t = TypeCode::InexactComplex;
                let v = self.pool_object(t, 2, 0)?;
                // SAFETY: two fields.
                unsafe {
                    put(v, Self::LAYOUT.field(t, 0), re.to_bits());
                    put(v, Self::LAYOUT.field(t, 1), im.to_bits());
                }
                Ok(v)
            }
            _ => unreachable!("vali-num has no other kinds of number"),
        }
    }

    /// A pool object of type `t` with two value fields.
    fn pool_pair_of(&mut self, t: TypeCode, a: Value, b: Value) -> Result<Value, OutOfMemory> {
        let v = self.pool_object(t, 2, 0)?;
        // SAFETY: two fields, and both values are valid and in the pool.
        unsafe {
            self.pool_store(v, Self::LAYOUT.field(t, 0), a);
            self.pool_store(v, Self::LAYOUT.field(t, 1), b);
        }
        Ok(v)
    }

    /// `constant` as a value, built into the pool. Pairs and vectors are
    /// built from a stack of their own, so a long or deeply nested quoted
    /// datum cannot overflow the native stack.
    pub(crate) fn pool_constant(&mut self, constant: &Constant) -> Result<Value, Error> {
        /// A pair or vector whose parts are being built.
        enum Frame<'a> {
            /// A list, built from the end: its last pair's `cdr` first,
            /// then each element from the last to the first, each made a
            /// pair at once.
            List {
                /// The elements not built yet, the next one last.
                cars: Vec<&'a Constant>,
                tail: &'a Constant,
                /// The list built so far, once its `cdr` is.
                rest: Option<Value>,
            },
            /// A vector's elements, built in order.
            Vector {
                items: &'a [Constant],
                values: Vec<Value>,
            },
        }
        let mut stack: Vec<Frame<'_>> = Vec::new();
        let mut built = None;
        let mut enter = Some(constant);
        loop {
            if let Some(constant) = enter.take() {
                match constant {
                    Constant::Pair(..) => {
                        // A list's spine is walked, not recursed into.
                        let mut cars = Vec::new();
                        let mut tail = constant;
                        while let Constant::Pair(car, cdr) = tail {
                            cars.push(&**car);
                            tail = cdr;
                        }
                        stack.push(Frame::List {
                            cars,
                            tail,
                            rest: None,
                        });
                    }
                    Constant::Vector(items) => stack.push(Frame::Vector {
                        items,
                        values: Vec::with_capacity(items.len()),
                    }),
                    leaf => built = Some(self.pool_leaf(leaf)?),
                }
            }
            let Some(top) = stack.last_mut() else {
                return Ok(built.expect("the constant built"));
            };
            if let Some(value) = built.take() {
                match top {
                    Frame::List { rest, .. } => match rest.take() {
                        // The `cdr`.
                        None => *rest = Some(value),
                        Some(cdr) => {
                            let v = self.pool_object(TypeCode::Pair, 2, 0)?;
                            // SAFETY: a pair has both fields; both values
                            // are valid.
                            unsafe {
                                self.pool_store(v, Self::LAYOUT.car(), value);
                                self.pool_store(v, Self::LAYOUT.cdr(), cdr);
                            }
                            *rest = Some(v);
                        }
                    },
                    Frame::Vector { values, .. } => values.push(value),
                }
            }
            let next = match top {
                Frame::List { cars, tail, rest } => match rest {
                    None => Some(*tail),
                    Some(_) => cars.pop(),
                },
                Frame::Vector { items, values } => items.get(values.len()),
            };
            if let Some(next) = next {
                enter = Some(next);
                continue;
            }
            built = Some(match stack.pop().expect("the top frame") {
                Frame::List { rest, .. } => rest.expect("a list's tail"),
                Frame::Vector { values, .. } => {
                    let v =
                        self.pool_object(TypeCode::Vector, 1 + values.len(), data::IMMUTABLE)?;
                    // SAFETY: the vector has a length word and an element
                    // for each value.
                    unsafe {
                        put(
                            v,
                            Self::LAYOUT.length(TypeCode::Vector),
                            values.len() as u64,
                        );
                        for (i, x) in values.iter().enumerate() {
                            self.pool_store(v, Self::LAYOUT.vector_element(i), *x);
                        }
                    }
                    v
                }
            });
        }
    }

    /// `constant`, which has no pairs or vectors in it to build, as a value
    /// built into the pool.
    fn pool_leaf(&mut self, constant: &Constant) -> Result<Value, Error> {
        Ok(match constant {
            Constant::Integer(n) => self.pool_integer(&Integer::from(*n))?,
            Constant::Flonum(x) => self.pool_flonum(*x)?,
            Constant::Boolean(b) => Value::boolean(*b),
            Constant::Character(c) => Value::character(*c),
            Constant::EmptyList => Value::EMPTY_LIST,
            Constant::Unspecified => Value::UNSPECIFIED,
            Constant::String(s) => {
                let bytes = self.pool_bytevector(s.as_bytes(), 0)?;
                let flags = data::IMMUTABLE | if s.is_ascii() { data::ASCII } else { 0 };
                let v = self.pool_object(TypeCode::String, 4, flags)?;
                // SAFETY: a string has these four fields; the hints are
                // complete, as there are none, and are never installed.
                unsafe {
                    put(v, Self::LAYOUT.string_byte_length(), s.len() as u64);
                    put(
                        v,
                        Self::LAYOUT.string_char_count(),
                        s.chars().count() as u64,
                    );
                    self.pool_store(v, Self::LAYOUT.string_bytes(), bytes);
                    put(v, Self::LAYOUT.string_hints(), Value::FALSE.raw());
                }
                v
            }
            // The isolate's own, interned and held for its life.
            Constant::Symbol(s) => self.intern(s)?,
            Constant::Keyword(s) => self.keyword_raw(s)?,
            Constant::Bignum { negative, limbs } => {
                self.pool_integer(&Integer::from_limbs(*negative, limbs))?
            }
            Constant::Bytevector(bytes) => self.pool_bytevector(bytes, data::IMMUTABLE)?,
            Constant::Ratio(..) | Constant::Complex(..) => {
                let n = crate::vm::constant_number(constant).ok_or_else(|| {
                    Error::scheme(format!("invalid numeric constant {constant:?}"))
                })?;
                self.pool_number(&n)?
            }
            Constant::Pair(..) | Constant::Vector(_) => {
                unreachable!("`pool_constant` builds the parts of pairs and vectors itself")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
        PreciseSideCounts,
    };

    fn list(n: i64) -> Constant {
        (0..n).fold(Constant::EmptyList, |rest, i| {
            Constant::Pair(Box::new(Constant::Integer(i)), Box::new(rest))
        })
    }

    /// Constants are frozen and heap pairs are not, on every strategy: the
    /// first word of a pool pair's page is set, that of a heap pair's is not.
    fn pairs_are_frozen_by_their_pages<S: Strategy>() {
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        // Enough pairs for several pool pages, each starting with a header
        // and so holding no object at its first word.
        let n = 3 * PAGE_BYTES as i64 / 16;
        let pool = cx.pool_constant(&list(n)).unwrap();
        let mut pages = std::collections::BTreeSet::new();
        let mut rest = pool;
        let mut count = 0;
        while rest.is_pair() {
            // SAFETY: a live pair of the pool.
            unsafe {
                assert!(Cx::<S>::pair_is_frozen(rest));
                assert_ne!(rest.address() as usize % PAGE_BYTES, 0);
                pages.insert(rest.address() as usize & !(PAGE_BYTES - 1));
                rest = cx.cdr(rest);
            }
            count += 1;
        }
        assert_eq!(count, n);
        assert!(pages.len() >= 3, "{} pages", pages.len());
        // SAFETY: valid values.
        let fresh = unsafe { cx.cons_raw(pool, Value::EMPTY_LIST) }.unwrap();
        // SAFETY: a live pair.
        assert!(!unsafe { Cx::<S>::pair_is_frozen(fresh) });
        // A heap pair may refer to a pool pair, and still be written.
        // SAFETY: as above.
        unsafe {
            cx.set_car(fresh, Value::TRUE);
            assert_eq!(cx.car(fresh), Value::TRUE);
        }
    }

    #[test]
    fn pairs_are_frozen_by_their_pages_under_every_strategy() {
        pairs_are_frozen_by_their_pages::<NoCollection>();
        pairs_are_frozen_by_their_pages::<DeferredHeadered>();
        pairs_are_frozen_by_their_pages::<DeferredSideCounts>();
        pairs_are_frozen_by_their_pages::<PreciseHeadered>();
        pairs_are_frozen_by_their_pages::<PreciseSideCounts>();
    }

    /// Every kind of constant builds and reads back as it was given, and
    /// the ones with a header are frozen by it, large ones too.
    fn every_constant_reads_back<S: Strategy>() {
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        let small = Constant::Vector(vec![
            Constant::Integer(7),
            Constant::Integer(i64::MAX),
            Constant::Flonum(1.5),
            Constant::String("text".into()),
            Constant::Symbol("sym".into()),
            Constant::Keyword("key".into()),
            Constant::Bignum {
                negative: true,
                limbs: vec![0, 1],
            },
            Constant::Ratio(
                Box::new(Constant::Integer(1)),
                Box::new(Constant::Integer(3)),
            ),
            Constant::Complex(
                Box::new(Constant::Flonum(1.0)),
                Box::new(Constant::Flonum(2.0)),
            ),
            list(3),
            Constant::Vector(vec![]),
            Constant::Bytevector(vec![1, 2]),
        ]);
        let v = cx.pool_constant(&small).unwrap();
        assert_eq!(
            cx.written(v),
            "#(7 9223372036854775807 1.5 \"text\" sym key: -18446744073709551616 1/3 1.0+2.0i (2 1 0) #() #u8(1 2))"
        );
        // SAFETY: live values of the pool.
        unsafe {
            assert_ne!(cx.header_flags(v, TypeCode::Vector) & data::IMMUTABLE, 0);
            for (i, t) in [
                (3, TypeCode::String),
                (10, TypeCode::Vector),
                (11, TypeCode::Bytevector),
            ] {
                let item = cx.vector_ref(v, i);
                assert_ne!(cx.header_flags(item, t) & data::IMMUTABLE, 0, "item {i}");
            }
        }

        // Objects over a page's worth, and a string with a multibyte
        // character, are as the constant says.
        let text = "wörd ".repeat(3_000);
        let bytes: Vec<u8> = (0..20_000).map(|i| i as u8).collect();
        let large = Constant::Vector(vec![
            Constant::String(text.clone().into()),
            Constant::Vector((0..5_000).map(Constant::Integer).collect()),
            Constant::Bytevector(bytes.clone()),
        ]);
        let v = cx.pool_constant(&large).unwrap();
        // SAFETY: live values of the pool.
        unsafe {
            assert_eq!(cx.string_str(cx.vector_ref(v, 0)), text);
            assert_eq!(cx.vector_length(cx.vector_ref(v, 1)), 5_000);
            assert_eq!(
                cx.vector_ref(cx.vector_ref(v, 1), 4_999).as_fixnum(),
                Some(4_999)
            );
            assert_eq!(cx.bytevector_bytes(cx.vector_ref(v, 2)), &bytes[..]);
            for (i, t) in [
                (0, TypeCode::String),
                (1, TypeCode::Vector),
                (2, TypeCode::Bytevector),
            ] {
                let item = cx.vector_ref(v, i);
                assert_ne!(
                    cx.header_flags(item, t) & data::IMMUTABLE,
                    0,
                    "large item {i}"
                );
            }
        }
    }

    #[test]
    fn every_constant_reads_back_under_every_strategy() {
        every_constant_reads_back::<NoCollection>();
        every_constant_reads_back::<DeferredHeadered>();
        every_constant_reads_back::<DeferredSideCounts>();
        every_constant_reads_back::<PreciseHeadered>();
        every_constant_reads_back::<PreciseSideCounts>();
    }

    /// Pool pages count toward the heap's limit, like its own.
    #[test]
    fn the_pool_is_charged_to_the_limit() {
        let mut cx = Cx::<NoCollection>::new(&HeapConfig {
            max_bytes: Some(2 * PAGE_BYTES),
        });
        assert_eq!(cx.bytes_used(), 0);
        cx.pool_constant(&list(10)).unwrap();
        assert_eq!(cx.bytes_used(), PAGE_BYTES);
        let too_many = list(2 * PAGE_BYTES as i64 / 16);
        assert!(cx.pool_constant(&too_many).is_err());
    }
}
