//! Ogg Vorbis streams made here, for what the decoder reads and no encoder
//! in use writes: residues of type 0, codebooks whose vectors are listed
//! one by one (lookup type 2), floors of type 0, and more channels than any
//! layout names.
//!
//! The streams are written from the specification, by a writer that knows
//! nothing of the decoder: its own bit packing, its own assignment of
//! codewords, and the layout of each residue type as section 8.6 describes
//! it. A stream says the same thing in several ways, and every way must
//! decode to the same samples, bit for bit: residues of type 0 and 2 like
//! type 1, which libvorbis writes and the tests of `lossy.rs` hold to the
//! source, and vectors listed one by one like the same vectors made from a
//! lattice.

mod common;

use common::vorbis::{Coding, random_values, stream};
use manguebyte_core::AudioBuffer;
use manguebyte_formats::vorbis::{self, VorbisError};

/// Samples in a block of the streams made here.
const BLOCK: usize = 256;

fn decoded(bytes: &[u8]) -> AudioBuffer<f32> {
    vorbis::read(bytes).unwrap().buffer
}

#[test]
fn every_residue_type_and_lookup_type_decodes_the_same_samples() {
    let values = random_values(6, 3, BLOCK, 1);
    let reference = decoded(&stream(
        3,
        &values,
        Coding::Residue {
            kind: 1,
            listed: false,
        },
    ));
    assert_eq!(reference.num_channels(), 3);
    assert_eq!(reference.num_frames(), 5 * BLOCK / 2);
    assert!(reference.peak() > 0.01, "the stream decodes to silence");
    for (kind, listed) in [(1, true), (0, false), (0, true), (2, false), (2, true)] {
        let bytes = stream(3, &values, Coding::Residue { kind, listed });
        let other = decoded(&bytes);
        assert!(
            other == reference,
            "residue type {kind}, listed vectors {listed}: different samples"
        );
    }
    // And a stream with other values decodes to other samples, so that the
    // equality above means something.
    let other = decoded(&stream(
        3,
        &random_values(6, 3, BLOCK, 2),
        Coding::Residue {
            kind: 1,
            listed: false,
        },
    ));
    assert!(other != reference);
}

#[test]
fn a_floor_of_type_0_is_refused_when_a_packet_uses_it() {
    let values = random_values(3, 1, BLOCK, 3);
    let bytes = stream(1, &values, Coding::Floor0);
    match vorbis::read(bytes.as_slice()) {
        Err(VorbisError::Unsupported(what)) => assert!(what.contains("type 0"), "{what}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_one_of_255_channels_is_decoded_as_its_own() {
    // 255 channels, the most Vorbis has, coded apart. Each must decode to
    // what the same values give as a stream of one channel: no channel
    // takes another's place, and none is lost.
    let channels = 255;
    let values = random_values(4, channels, BLOCK, 4);
    for kind in [1, 2] {
        let many = decoded(&stream(
            channels,
            &values,
            Coding::Residue {
                kind,
                listed: false,
            },
        ));
        assert_eq!(many.num_channels(), channels);
        for channel in [0, 1, 2, 7, 8, 100, 254] {
            let alone: Vec<Vec<Vec<i32>>> = values
                .iter()
                .map(|packet| vec![packet[channel].clone()])
                .collect();
            let one = decoded(&stream(
                1,
                &alone,
                Coding::Residue {
                    kind: 1,
                    listed: false,
                },
            ));
            assert!(
                many.channel(channel) == one.channel(0),
                "residue type {kind}, channel {channel}"
            );
        }
    }
}
