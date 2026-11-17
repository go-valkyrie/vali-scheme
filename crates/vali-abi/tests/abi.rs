//! The encodings of ADR 0002, checked exhaustively where the space is small
//! and on generated values where it is not.

use vali_abi::immediate::{self, Marker, Subtag};
use vali_abi::{Header, Layout, StrategyLayout, Tag, TypeCode, data, fixnum};

/// A small deterministic generator, so failures reproduce.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
}

#[test]
fn tags() {
    for bits in 0..8u64 {
        let tag = Tag::of(0xDEAD_BEE0 | bits);
        assert_eq!(tag as u64, bits);
        assert_eq!(tag.is_pointer(), !matches!(bits, 0b000 | 0b111));
    }
    let tagged = Tag::String.tag_address(0x1000);
    assert_eq!(Tag::of(tagged), Tag::String);
    assert_eq!(Tag::address_of(tagged), 0x1000);
}

#[test]
fn fixnums() {
    for n in [0, 1, -1, 42, fixnum::MIN, fixnum::MAX] {
        let word = fixnum::encode(n).unwrap();
        assert!(fixnum::is_fixnum(word));
        assert_eq!(fixnum::decode(word), n);
    }
    assert_eq!(fixnum::MAX, (1 << 60) - 1);
    assert_eq!(fixnum::encode(fixnum::MAX + 1), None);
    assert_eq!(fixnum::encode(fixnum::MIN - 1), None);
    assert_eq!(fixnum::encode(i64::MIN), None);
}

#[test]
fn fixnum_words_add_and_subtract_directly() {
    let mut rng = Lcg(1);
    let range = (fixnum::MAX as u64) * 2 + 1;
    for _ in 0..10_000 {
        let a = (rng.next() % range) as i64 + fixnum::MIN;
        let b = (rng.next() % range) as i64 + fixnum::MIN;
        let (wa, wb) = (fixnum::encode(a).unwrap(), fixnum::encode(b).unwrap());
        if let Some(sum) = fixnum::encode(a + b) {
            assert_eq!(wa.wrapping_add(wb), sum);
        }
        if let Some(difference) = fixnum::encode(a - b) {
            assert_eq!(wa.wrapping_sub(wb), difference);
        }
        assert_eq!((wa as i64) < (wb as i64), a < b);
    }
}

#[test]
fn the_immediate_words_of_adr_0002() {
    assert_eq!(immediate::FALSE, 0x0F);
    assert_eq!(immediate::TRUE, 0x10F);
    assert_eq!(immediate::EMPTY_LIST, 0x17);
    assert_eq!(immediate::EOF, 0x1F);
    assert_eq!(immediate::UNSPECIFIED, 0x27);
    assert_eq!(immediate::DEFAULT, 0x2F);
    assert_eq!(immediate::marker(Marker::Unbound), 0x37);
    assert_eq!(immediate::marker(Marker::Tombstone), 2 << 8 | 0x37);
    assert_eq!(immediate::character('A'), 0x41 << 8 | 0x07);
}

#[test]
fn immediate_subtags() {
    let cases = [
        (immediate::character('λ'), Subtag::Character),
        (immediate::FALSE, Subtag::Boolean),
        (immediate::TRUE, Subtag::Boolean),
        (immediate::EMPTY_LIST, Subtag::EmptyList),
        (immediate::EOF, Subtag::Eof),
        (immediate::UNSPECIFIED, Subtag::Unspecified),
        (immediate::DEFAULT, Subtag::Default),
        (immediate::marker(Marker::Uninitialised), Subtag::Marker),
    ];
    for (word, subtag) in cases {
        assert_eq!(Tag::of(word), Tag::Immediate);
        assert_eq!(immediate::subtag(word), Some(subtag));
    }
    assert_eq!(immediate::subtag(7 << 3 | 0b111), None, "reserved");
    assert_eq!(immediate::subtag(fixnum::encode(5).unwrap()), None);
    assert_eq!(immediate::payload(immediate::TRUE), 1);
}

#[test]
fn booleans_are_one_mask_and_compare() {
    assert!(immediate::is_boolean(immediate::boolean(true)));
    assert!(immediate::is_boolean(immediate::boolean(false)));
    for word in [
        immediate::EMPTY_LIST,
        immediate::make(Subtag::Boolean, 2),
        immediate::make(Subtag::Boolean, 3),
        immediate::character('\u{1}'),
        fixnum::encode(0).unwrap(),
        0x10E,
    ] {
        assert!(!immediate::is_boolean(word), "{word:#x}");
    }
}

#[test]
fn every_character_round_trips() {
    for scalar in 0..=char::MAX as u32 {
        if let Some(c) = char::from_u32(scalar) {
            let word = immediate::character(c);
            assert_eq!(immediate::to_character(word), Some(c));
        } else {
            let word = immediate::make(Subtag::Character, u64::from(scalar));
            assert_eq!(immediate::to_character(word), None, "{scalar:#x}");
        }
    }
    let too_big = immediate::make(Subtag::Character, 1 << 32 | 0x41);
    assert_eq!(immediate::to_character(too_big), None);
    assert_eq!(immediate::to_character(immediate::TRUE), None);
}

#[test]
fn type_codes_are_odd_and_round_trip() {
    for (i, &t) in TypeCode::ALL.iter().enumerate() {
        assert_eq!(t as usize, 2 * i + 1);
        assert_eq!(TypeCode::from_u8(t as u8), Some(t));
    }
    assert_eq!(TypeCode::from_u8(0), None);
    assert_eq!(TypeCode::from_u8(2), None);
    assert_eq!(TypeCode::from_u8(47), None);
    assert_eq!(TypeCode::Pair.tag(), Tag::Pair);
    assert_eq!(TypeCode::Vector.tag(), Tag::Other);
}

#[test]
fn headers() {
    let header = Header::new(TypeCode::String, data::IMMUTABLE | data::ASCII).unwrap();
    assert!(Header::is_header(header.0));
    assert_eq!(header.type_code(), Some(TypeCode::String));
    assert_eq!(header.data(), data::IMMUTABLE | data::ASCII);
    assert_eq!(header.strategy_half(), 0);

    let counted = header.with_strategy_half(0x8000_0001);
    assert_eq!(counted.strategy_half(), 0x8000_0001);
    assert_eq!(counted.type_code(), Some(TypeCode::String));
    assert_eq!(counted.data(), header.data());

    let widened = counted.with_data(data::IMMUTABLE).unwrap();
    assert_eq!(widened.data(), data::IMMUTABLE);
    assert_eq!(widened.strategy_half(), 0x8000_0001);

    assert!(Header::new(TypeCode::Closure, Header::DATA_MAX).is_some());
    assert_eq!(Header::new(TypeCode::Closure, Header::DATA_MAX + 1), None);
    assert_eq!(header.with_data(1 << 24), None);
}

#[test]
fn forwarding_pointers_are_never_headers() {
    let mut rng = Lcg(7);
    for _ in 0..1000 {
        let address = rng.next() & !7;
        assert!(!Header::is_header(address));
    }
    for &t in &TypeCode::ALL {
        let header = Header::new(t, 0).unwrap().with_strategy_half(u32::MAX);
        assert!(Header::is_header(header.0));
    }
}

#[test]
fn layouts_with_and_without_pair_headers() {
    let compact = Layout::new(StrategyLayout::COMPACT);
    assert_eq!((compact.car(), compact.cdr()), (-1, 7));
    assert_eq!(compact.header(TypeCode::Pair), None);
    assert_eq!(compact.words(TypeCode::Pair, 2), 2);
    assert_eq!(compact.flonum(), -5);

    let headered = Layout::new(StrategyLayout::HEADERED);
    assert_eq!((headered.car(), headered.cdr()), (7, 15));
    assert_eq!(headered.header(TypeCode::Pair), Some(-1));
    assert_eq!(headered.words(TypeCode::Pair, 2), 3);

    for layout in [compact, headered] {
        assert_eq!(layout.closure_code(), 6);
        assert_eq!(layout.closure_free(0), 14);
        assert_eq!(layout.length(TypeCode::Vector), 2);
        assert_eq!(layout.vector_element(0), 10);
        assert_eq!(layout.record_type(), 2);
        assert_eq!(layout.record_field(1), 18);
        assert_eq!(layout.string_hints(), 28);
        assert_eq!(layout.symbol_hash(), 13);
    }
}

#[test]
fn every_field_is_word_aligned() {
    for strategy in [StrategyLayout::COMPACT, StrategyLayout::HEADERED] {
        let layout = Layout::new(strategy);
        for &t in &TypeCode::ALL {
            for index in 0..4 {
                let address = 0x1000 + i64::from(layout.field(t, index)) + t.tag() as i64;
                assert_eq!(address % 8, 0, "{t:?} field {index}");
            }
            if let Some(offset) = layout.header(t) {
                assert_eq!(offset + t.tag() as i32, 0, "{t:?} header");
            }
        }
    }
}
