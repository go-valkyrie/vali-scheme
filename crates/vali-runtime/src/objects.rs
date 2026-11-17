//! What heap objects hold (ADR 0003): each object's type, its size, and the
//! fields that hold values. Freeing an object, cycle collection and tracing
//! all walk this one description. It needs nothing but the layout, so a
//! strategy can use it while the runtime lends it the heap.

use vali_abi::{Header, Tag, TypeCode};

use crate::cx::{Cx, get};
use crate::strategy::Strategy;
use crate::value::Value;

impl<S: Strategy> Cx<S> {
    /// The type of heap object `v` is, or `None` for a fixnum or an
    /// immediate.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub(crate) unsafe fn object_type(v: Value) -> Option<TypeCode> {
        match v.tag() {
            Tag::Pair => Some(TypeCode::Pair),
            Tag::Flonum => Some(TypeCode::Flonum),
            Tag::Closure => Some(TypeCode::Closure),
            Tag::Symbol => Some(TypeCode::Symbol),
            Tag::String => Some(TypeCode::String),
            Tag::Other => {
                let offset = Self::LAYOUT.header(TypeCode::Record)?;
                // SAFETY: passed on to the caller: an "other" object starts
                // with its header.
                Header(unsafe { get(v, offset) }).type_code()
            }
            Tag::Fixnum | Tag::Immediate => None,
        }
    }

    /// The runtime data in the header of `v`, of type `t`.
    ///
    /// # Safety
    ///
    /// `v` must be a live object of type `t`, which has a header.
    unsafe fn header_data(v: Value, t: TypeCode) -> usize {
        Self::LAYOUT.header(t).map_or(0, |offset| {
            // SAFETY: passed on to the caller.
            Header(unsafe { get(v, offset) }).data() as usize
        })
    }

    /// How many words `v`, of type `t`, has after its header.
    ///
    /// # Safety
    ///
    /// `v` must be a live object of type `t`.
    pub(crate) unsafe fn object_fields(v: Value, t: TypeCode) -> usize {
        // SAFETY: passed on to the caller; each length word is where the
        // object's type keeps it.
        unsafe {
            match t {
                TypeCode::Pair | TypeCode::RecordType => 2,
                TypeCode::Flonum | TypeCode::Box | TypeCode::Syntax => 1,
                TypeCode::Symbol => 2,
                TypeCode::String => 4,
                TypeCode::Closure => 1 + Self::header_data(v, t),
                TypeCode::Record => 1 + Self::header_data(v, t),
                TypeCode::Vector => 1 + get(v, Self::LAYOUT.length(t)) as usize,
                TypeCode::Bytevector => 1 + (get(v, Self::LAYOUT.length(t)) as usize).div_ceil(8),
                TypeCode::Bignum => 1 + get(v, Self::LAYOUT.length(t)) as usize,
                other => unreachable!("no {other:?} objects are allocated yet"),
            }
        }
    }

    /// Calls `f` with each value `v`, of type `t`, holds: fixnums and
    /// immediates included, which callers that want objects skip.
    ///
    /// # Safety
    ///
    /// `v` must be a live object of type `t`.
    pub(crate) unsafe fn for_each_child(v: Value, t: TypeCode, mut f: impl FnMut(Value)) {
        let field = |i: usize| -> Value {
            // SAFETY: the caller guarantees a live object of type `t`, and
            // `i` is one of its value-holding fields.
            Value::from_raw(unsafe { get(v, Self::LAYOUT.field(t, i)) })
        };
        // SAFETY: as above.
        let fields = unsafe { Self::object_fields(v, t) };
        match t {
            TypeCode::Pair | TypeCode::RecordType | TypeCode::Box | TypeCode::Record => {
                (0..fields).for_each(|i| f(field(i)));
            }
            // The first word is the code pointer, not a value.
            TypeCode::Closure | TypeCode::Vector => (1..fields).for_each(|i| f(field(i))),
            // The name; the hash is a number.
            TypeCode::Symbol => f(field(0)),
            // The bytes and the index hints.
            TypeCode::String => {
                f(field(2));
                f(field(3));
            }
            _ => {}
        }
    }

    /// `v`'s size in words, its header included, with the objects it refers
    /// to pushed onto `children`: what freeing and cycle collection need.
    ///
    /// # Safety
    ///
    /// `v` must be a live object.
    pub(crate) unsafe fn describe(v: Value, children: &mut Vec<Value>) -> usize {
        // SAFETY: passed on to the caller.
        unsafe {
            let t = Self::object_type(v).expect("an object");
            Self::for_each_child(v, t, |c| {
                if c.is_pointer() {
                    children.push(c);
                }
            });
            Self::LAYOUT.words(t, Self::object_fields(v, t))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Cx, Handle, HeapConfig, NoCollection, Value};
    use vali_abi::TypeCode;

    type C = Cx<NoCollection>;

    fn children(v: Value) -> (TypeCode, usize, Vec<Value>) {
        // SAFETY: the callers pass live values.
        unsafe {
            let t = C::object_type(v).unwrap();
            let mut out = Vec::new();
            C::for_each_child(v, t, |c| out.push(c));
            (t, C::object_fields(v, t), out)
        }
    }

    #[test]
    fn objects_describe_their_values() {
        let mut cx = C::new(&HeapConfig::default());
        let one = Handle::fixnum(1).unwrap();
        let two = Handle::fixnum(2).unwrap();
        let pair = cx.cons(&one, &two).unwrap();
        let pair = cx.get(&pair).unwrap();
        let (t, fields, kids) = children(pair);
        assert_eq!((t, fields), (TypeCode::Pair, 2));
        assert_eq!(kids, [Value::fixnum(1).unwrap(), Value::fixnum(2).unwrap()]);

        let vector = cx.read("#(a b c)").unwrap().remove(0);
        let vector = cx.get(&vector).unwrap();
        let (t, fields, kids) = children(vector);
        assert_eq!((t, fields, kids.len()), (TypeCode::Vector, 4, 3));
        assert!(kids.iter().all(|k| k.tag() == vali_abi::Tag::Symbol));

        let string = cx.read("\"hello, world\"").unwrap().remove(0);
        let string = cx.get(&string).unwrap();
        let (t, fields, kids) = children(string);
        assert_eq!((t, fields), (TypeCode::String, 4));
        let (t, fields, kids) = children(kids[0]);
        assert_eq!((t, fields, kids.len()), (TypeCode::Bytevector, 3, 0));
    }
}
