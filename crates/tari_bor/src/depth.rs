//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Codec-wide nesting bound applied to untrusted input before any typed decode runs.

#[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
use alloc::vec::Vec;

use minicbor::decode;
#[cfg(not(target_arch = "wasm32"))]
use minicbor::{Decoder, data::Type};

/// Maximum container nesting accepted by [`crate::decode`] and its siblings.
///
/// Nesting is a property of the input bytes, so this is the only bound that covers every target
/// type. A `#[derive(Decode)]` on a self-recursive type descends one native stack frame per level
/// and has no way to thread a counter of its own, so without this bound a payload of a few hundred
/// bytes can drive the decoder off the end of the stack — a guard-page abort of the whole process,
/// which no caller can catch.
///
/// It is set generously above anything a legitimate payload nests to, because rejecting a valid
/// payload is the worse failure: semantic depth limits are the types' own business (see
/// [`crate::MAX_DECODE_DEPTH`] for the dynamic `Value` tree), and those apply to a subtree whose
/// own nesting starts at zero, while this one counts from the top of the whole payload. What this
/// bound must guarantee is that the deepest accepted input still decodes within the smallest stack
/// untrusted decode runs on — a 2 MiB tokio worker.
pub const MAX_NESTING_DEPTH: usize = 256;

/// Rejects input nested deeper than [`MAX_NESTING_DEPTH`].
///
/// The walk is iterative and reads only item heads, so it costs a fraction of the decode it guards
/// and is itself immune to the recursion it rejects.
pub fn check_nesting_depth(input: &[u8]) -> Result<(), decode::Error> {
    if within_bound(input) {
        return Ok(());
    }
    Err(decode::Error::message("maximum CBOR nesting depth exceeded"))
}

/// On `wasm32` the bound is the embedder's to enforce: running off the stack there is a trap it
/// reports as an error, and this crate is linked into every template, where the walk's cost would
/// be metered onto each of the guest's own decodes.
#[cfg(target_arch = "wasm32")]
fn within_bound(_input: &[u8]) -> bool {
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn within_bound(input: &[u8]) -> bool {
    walk(&mut Decoder::new(input)).unwrap_or(true)
}

/// Walks the heads of the first item in `d`, returning whether it stays within
/// [`MAX_NESTING_DEPTH`].
///
/// Malformed input is reported as `Err` and treated by the caller as within bound: the decode that
/// follows produces a parse error far more specific than anything this walk could say.
#[cfg(not(target_arch = "wasm32"))]
fn walk(d: &mut Decoder<'_>) -> Result<bool, decode::Error> {
    // Items still to read per open container, innermost last: `Some(n)` for a definite-length
    // container, `None` for an indefinite-length one that ends at a break byte. The outermost frame
    // is the single top-level item, so an item's nesting depth is `stack.len() - 1`.
    let mut stack: Vec<Option<u64>> = Vec::with_capacity(16);
    stack.push(Some(1));

    loop {
        let Some(frame) = stack.last().copied() else {
            return Ok(true);
        };
        match frame {
            Some(0) => {
                stack.pop();
                continue;
            },
            Some(_) => {},
            None => {
                if matches!(d.datatype()?, Type::Break) {
                    d.skip()?;
                    stack.pop();
                    continue;
                }
            },
        }

        // An item is about to be read, and the frames open around it are its nesting. Checked here
        // rather than where a container is opened, so an empty container at the bound is accepted
        // exactly as a scalar there is.
        if stack.len() - 1 > MAX_NESTING_DEPTH {
            return Ok(false);
        }

        if let Some(Some(n)) = stack.last_mut() {
            *n -= 1;
        }

        let remaining = match d.datatype()? {
            Type::Array | Type::ArrayIndef => d.array()?,
            // A map's key and value each nest at the same depth, so one frame covers both.
            Type::Map | Type::MapIndef => d.map()?.map(|n| n.saturating_mul(2)),
            Type::Tag => {
                d.tag()?;
                Some(1)
            },
            // Scalars, including indefinite-length byte and text strings, whose chunks do not nest.
            _ => {
                d.skip()?;
                continue;
            },
        };

        stack.push(remaining);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn scalars_and_flat_containers_pass() {
        for input in [
            &[0x00][..],                                                 // 0
            &[0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..], // a 64-bit negative
            &[0x80][..],                                                 // []
            &[0xa0][..],                                                 // {}
            &[0x83, 0x01, 0x02, 0x03][..],                               // [1, 2, 3]
            &[0xa1, 0x01, 0x02][..],                                     // {1: 2}
            &[0x9f, 0x01, 0xff][..],                                     // [_ 1]
            &[0xbf, 0x01, 0x02, 0xff][..],                               // {_ 1: 2}
            &[0x43, 0x01, 0x02, 0x03][..],                               // h'010203'
            &[0x5f, 0x41, 0x01, 0x41, 0x02, 0xff][..],                   // (_ h'01', h'02')
            &[0xc0, 0x01][..],                                           // 0(1)
        ] {
            assert!(check_nesting_depth(input).is_ok(), "rejected {input:x?}");
        }
    }

    #[test]
    fn trailing_bytes_after_the_first_item_are_not_walked() {
        // A flat item followed by a nesting bomb: only the first item is this walk's business.
        let mut input = vec![0x00];
        input.extend(core::iter::repeat_n(0x81u8, 10_000));
        assert!(check_nesting_depth(&input).is_ok());
    }

    #[test]
    fn nesting_at_the_bound_passes_and_one_level_deeper_is_rejected() {
        for head in [
            0x81u8, // array of 1
            0xa1,   // map of 1
            0x9f,   // indefinite array
            0xc0,   // tag
        ] {
            let at_bound: Vec<u8> = core::iter::repeat_n(head, MAX_NESTING_DEPTH)
                .chain(core::iter::once(0x00))
                .collect();
            assert!(check_nesting_depth(&at_bound).is_ok(), "head {head:#x} at bound");

            let over_bound: Vec<u8> = core::iter::repeat_n(head, MAX_NESTING_DEPTH + 1)
                .chain(core::iter::once(0x00))
                .collect();
            assert!(check_nesting_depth(&over_bound).is_err(), "head {head:#x} over bound");
        }
    }

    #[test]
    fn an_empty_container_at_the_bound_is_accepted_as_a_scalar_there_is() {
        let containers: Vec<u8> = core::iter::repeat_n(0x81u8, MAX_NESTING_DEPTH).collect();
        for innermost in [0x00u8, 0x80, 0xa0] {
            let mut input = containers.clone();
            input.push(innermost);
            assert!(check_nesting_depth(&input).is_ok(), "innermost {innermost:#x}");
        }
    }

    #[test]
    fn a_nesting_bomb_is_rejected_without_recursing() {
        let bomb = vec![0x81u8; 1_000_000];
        assert!(check_nesting_depth(&bomb).is_err());
    }

    #[test]
    fn malformed_input_is_left_for_the_decoder_to_reject() {
        for input in [
            &[][..],     // no input at all
            &[0x81][..], // array of 1 with no element
            &[0xff][..], // a stray break
            &[0x1c][..], // a reserved additional-information value
        ] {
            assert!(check_nesting_depth(input).is_ok(), "claimed {input:x?} for itself");
        }
    }
}
