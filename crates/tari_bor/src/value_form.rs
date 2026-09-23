//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Checking that CBOR bytes have a [`Value`] form, without decoding them into one.

use minicbor::{Decoder, data::Type, decode};

use crate::{BorError, MAX_DECODE_DEPTH, Value};

/// Whether `input` is exactly one CBOR item that decodes as a [`Value`], checked without building it.
///
/// Accepts exactly what `decode_exact::<Value>` accepts: the walk reads each item the way
/// [`Value`]'s decoder does and discards it, so it allocates nothing. A value that passes has a
/// [`Value`] form, and with it a serde (JSON) form.
pub fn check_value_form(input: &[u8]) -> Result<(), BorError> {
    let mut d = Decoder::new(input);
    check_value(&mut d, 0)?;
    if d.position() != input.len() {
        return Err(BorError::new("trailing bytes after the CBOR item".into()));
    }
    Ok(())
}

fn check_value(d: &mut Decoder<'_>, depth: usize) -> Result<(), decode::Error> {
    if depth >= MAX_DECODE_DEPTH {
        return Err(decode::Error::message("maximum CBOR nesting depth exceeded"));
    }
    match d.datatype()? {
        Type::Null => d.null(),
        Type::Undefined => d.undefined(),
        Type::Bool => d.bool().map(drop),
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => d.u64().map(drop),
        Type::I8 | Type::I16 | Type::I32 | Type::I64 => d.i64().map(drop),
        Type::Int => match Value::integer(i128::from(d.int()?)) {
            Some(_) => Ok(()),
            None => Err(decode::Error::message("Value::Integer out of CBOR range")),
        },
        Type::F16 | Type::F32 | Type::F64 => d.f64().map(drop),
        Type::Bytes => d.bytes().map(drop),
        Type::BytesIndef => d.bytes_iter()?.try_for_each(|chunk| chunk.map(drop)),
        Type::String => d.str().map(drop),
        Type::StringIndef => d.str_iter()?.try_for_each(|chunk| chunk.map(drop)),
        Type::Array | Type::ArrayIndef => {
            let len = d.array()?;
            check_items(d, len, depth, 1)
        },
        Type::Map | Type::MapIndef => {
            let len = d.map()?;
            check_items(d, len, depth, 2)
        },
        Type::Tag => {
            d.tag()?;
            check_value(d, depth + 1)
        },
        other => Err(decode::Error::message("unsupported CBOR datatype").with_message(other)),
    }
}

/// The entries of a container opened at `depth`, each `per_entry` items: one for an array, a key
/// and a value for a map. An indefinite-length container ends at a break before an entry.
fn check_items(d: &mut Decoder<'_>, len: Option<u64>, depth: usize, per_entry: usize) -> Result<(), decode::Error> {
    let check_entry = |d: &mut Decoder<'_>| (0..per_entry).try_for_each(|_| check_value(d, depth + 1));
    match len {
        Some(n) => (0..n).try_for_each(|_| check_entry(d)),
        None => loop {
            if matches!(d.datatype()?, Type::Break) {
                return d.skip();
            }
            check_entry(d)?;
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_every_kind_of_value() {
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Integer(i128::from(i64::MIN)),
            Value::Integer(i128::from(u64::MAX)),
            Value::Float(1.5),
            Value::Bytes(vec![1, 2, 3]),
            Value::Text("text".to_string()),
            Value::Array(vec![Value::Integer(1), Value::Array(vec![])]),
            Value::Map(vec![(Value::Text("k".to_string()), Value::Map(vec![]))]),
            Value::Tag(129, Box::new(Value::Bytes(vec![0; 32]))),
        ] {
            let bytes = crate::encode(&value).unwrap();
            assert!(check_value_form(&bytes).is_ok(), "rejected {value:?}");
        }
    }

    #[test]
    fn accepts_indefinite_length_items() {
        for input in [
            &[0x5f, 0x41, 0x01, 0x41, 0x02, 0xff][..], // (_ h'01', h'02')
            &[0x7f, 0x61, 0x61, 0x61, 0x62, 0xff][..], // (_ "a", "b")
            &[0x9f, 0x01, 0x02, 0xff][..],             // [_ 1, 2]
            &[0xbf, 0x01, 0x02, 0xff][..],             // {_ 1: 2}
        ] {
            assert!(check_value_form(input).is_ok(), "rejected {input:02x?}");
        }
    }

    #[test]
    fn rejects_simple_values() {
        assert!(check_value_form(&[0xe0]).is_err());
        assert!(check_value_form(&[0xf8, 0x20]).is_err());
    }

    #[test]
    fn rejects_a_negative_integer_below_i64_min() {
        assert!(check_value_form(&[0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_ok());
        assert!(check_value_form(&[0x3b, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]).is_err());
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert!(check_value_form(&[0x62, 0xff, 0xfe]).is_err());
        assert!(check_value_form(&[0x7f, 0x62, 0xff, 0xfe, 0xff]).is_err());
    }

    #[test]
    fn rejects_an_indefinite_chunk_of_the_wrong_type() {
        assert!(check_value_form(&[0x7f, 0x41, 0x00, 0xff]).is_err());
        assert!(check_value_form(&[0x5f, 0x61, 0x61, 0xff]).is_err());
    }

    #[test]
    fn rejects_an_indefinite_map_with_a_key_and_no_value() {
        assert!(check_value_form(&[0xbf, 0x01, 0xff]).is_err());
    }

    #[test]
    fn rejects_truncated_and_stray_items() {
        for input in [
            &[][..],           // nothing
            &[0x82, 0x01][..], // [1, <missing>]
            &[0x9f, 0x01][..], // [_ 1 <no break>
            &[0xc0][..],       // a tag with no item
            &[0xff][..],       // a stray break
            &[0x1c][..],       // a reserved head
        ] {
            assert!(check_value_form(input).is_err(), "accepted {input:02x?}");
        }
    }

    #[test]
    fn rejects_trailing_bytes() {
        assert!(check_value_form(&[0x00]).is_ok());
        assert!(check_value_form(&[0x00, 0x00]).is_err());
    }

    #[test]
    fn nesting_below_the_bound_passes_and_at_it_is_rejected() {
        for head in [0x81u8, 0xa1, 0x9f, 0xc0] {
            let nested = |levels: usize| {
                let mut input: Vec<u8> = core::iter::repeat_n(head, levels).collect();
                input.push(0x00);
                if head == 0xa1 {
                    // Each map level needs its key as well as the nested value.
                    input = core::iter::repeat_n([0xa1u8, 0x00], levels)
                        .flatten()
                        .chain([0x00])
                        .collect();
                }
                if head == 0x9f {
                    input.extend(core::iter::repeat_n(0xffu8, levels));
                }
                input
            };
            assert!(
                check_value_form(&nested(MAX_DECODE_DEPTH - 1)).is_ok(),
                "head {head:#x} below the bound"
            );
            assert!(
                check_value_form(&nested(MAX_DECODE_DEPTH)).is_err(),
                "head {head:#x} at the bound"
            );
        }
    }

    /// `check_value_form` must accept exactly what decoding a `Value` accepts: a value it passes is
    /// served as JSON on the strength of that.
    fn assert_agrees_with_decode(input: &[u8]) {
        let decoded = crate::decode_exact::<Value>(input).is_ok();
        let checked = check_value_form(input).is_ok();
        assert_eq!(
            checked, decoded,
            "check_value_form disagrees with the decoder on {input:02x?}"
        );
    }

    #[test]
    fn check_value_form_agrees_with_decode_on_edge_cases() {
        let deep = |n: usize| {
            core::iter::repeat_n(0x81u8, n)
                .chain(core::iter::once(0x00))
                .collect::<Vec<_>>()
        };
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00],
            vec![0xe0],                                                 // simple(0)
            vec![0xf8, 0x20],                                           // simple(32)
            vec![0xf4],                                                 // false
            vec![0xf6],                                                 // null
            vec![0xf7],                                                 // undefined
            vec![0xff],                                                 // stray break
            vec![0x1c],                                                 // reserved
            vec![0xf9, 0x3c, 0x00],                                     // f16
            vec![0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // i64::MIN
            vec![0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // -2^64
            vec![0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // u64::MAX
            vec![0x62, 0xff, 0xfe],                                     // invalid UTF-8
            vec![0x7f, 0x61, 0x61, 0xff],                               // (_ "a")
            vec![0x7f, 0x41, 0x00, 0xff],                               // text chunk that is bytes
            vec![0x5f, 0x41, 0x00, 0xff],                               // (_ h'00')
            vec![0x5f, 0x61, 0x61, 0xff],                               // bytes chunk that is text
            vec![0xbf, 0x01, 0x02, 0xff],                               // {_ 1: 2}
            vec![0xbf, 0x01, 0xff],                                     // {_ 1} (no value)
            vec![0x9f, 0x01, 0xff],                                     // [_ 1]
            vec![0x9f, 0x01],                                           // [_ 1 (no break)
            vec![0x82, 0x01],                                           // [1 (short)
            vec![0xc2, 0x41, 0x01],                                     // 2(h'01')
            vec![0xc0],                                                 // tag with no item
            vec![0x00, 0x00],                                           // trailing byte
            deep(MAX_DECODE_DEPTH - 1),
            deep(MAX_DECODE_DEPTH),
            core::iter::repeat_n(0xc0u8, MAX_DECODE_DEPTH - 1)
                .chain(core::iter::once(0x00))
                .collect(),
            core::iter::repeat_n(0xc0u8, MAX_DECODE_DEPTH)
                .chain(core::iter::once(0x00))
                .collect(),
        ];
        for case in &cases {
            assert_agrees_with_decode(case);
        }
    }

    #[test]
    fn check_value_form_agrees_with_decode_on_arbitrary_bytes() {
        // Bytes drawn mostly from item heads, so short inputs reach containers, tags, chunks and
        // simple values often.
        const HEADS: [u8; 24] = [
            0x00, 0x18, 0x20, 0x3b, 0x41, 0x5f, 0x61, 0x62, 0x7f, 0x80, 0x81, 0x82, 0x9f, 0xa0, 0xa1, 0xbf, 0xc0, 0xc2,
            0xe0, 0xf4, 0xf7, 0xf9, 0xfb, 0xff,
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            let len = (next() % 12) as usize;
            let input: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    if r % 4 == 0 {
                        r as u8
                    } else {
                        HEADS[(r >> 8) as usize % HEADS.len()]
                    }
                })
                .collect();
            assert_agrees_with_decode(&input);
        }
    }
}
