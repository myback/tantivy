use std::io;

use crate::common::{BinarySerializable, VInt};
use crate::directory::OwnedBytes;
use crate::positions::COMPRESSION_BLOCK_SIZE;
use crate::postings::compression::{BlockDecoder, VIntDecoder};

/// When accessing the position of a term, we get a positions_idx from the `Terminfo`.
/// This means we need to skip to the `nth` positions efficiently.
///
/// Blocks are compressed using bitpacking, so `skip_read` contains the number of bits
/// (values can go from 0bit to 32 bits) required to decompress every block.
///
/// A given block obviously takes `(128 x  num_bit_for_the_block / num_bits_in_a_byte)`,
/// so skipping a block without decompressing it is just a matter of advancing that many
/// bytes.

#[derive(Clone)]
pub struct PositionReader {
    bit_widths: OwnedBytes,
    positions: OwnedBytes,

    block_decoder: BlockDecoder,

    // offset, expressed in positions, for the first position of the block currently loaded
    // block_offset is a multiple of COMPRESSION_BLOCK_SIZE.
    block_offset: u64,
    // offset, expressed in positions, for the position of the first block encoded
    // in the `self.positions` bytes, and if bitpacked, compressed using the bitwidth in
    // `self.bit_widths`.
    //
    // As we advance, anchor increases simultaneously with bit_widths and positions get consumed.
    anchor_offset: u64,
}

impl PositionReader {
    pub fn new(mut positions_data: OwnedBytes) -> io::Result<PositionReader> {
        let num_positions_bitpacked_blocks = VInt::deserialize(&mut positions_data)?.0 as usize;
        let (bit_widths, positions) = positions_data.split(num_positions_bitpacked_blocks);
        Ok(PositionReader {
            bit_widths,
            positions,
            block_decoder: BlockDecoder::default(),
            block_offset: std::i64::MAX as u64,
            anchor_offset: 0u64,
        })
    }

    fn advance_num_blocks(&mut self, num_blocks: usize) {
        let num_bits: usize = self.bit_widths.as_ref()[..num_blocks]
            .iter()
            .cloned()
            .map(|num_bits| num_bits as usize)
            .sum();
        let num_bytes_to_skip = num_bits * COMPRESSION_BLOCK_SIZE / 8;
        self.bit_widths.advance(num_blocks as usize);
        self.positions.advance(num_bytes_to_skip);
        self.anchor_offset += (num_blocks * COMPRESSION_BLOCK_SIZE) as u64;
    }

    /// block_rel_id is counted relatively to the anchor.
    /// block_rel_id = 0 means the anchor block.
    /// block_rel_id = i means the ith block after the anchor block.
    fn load_block(&mut self, block_rel_id: usize) {
        let bit_widths = self.bit_widths.as_slice();
        let byte_offset: usize = bit_widths[0..block_rel_id].iter().map(|&b| b as usize).sum::<usize>() * COMPRESSION_BLOCK_SIZE / 8;
        let compressed_data = &self.positions.as_slice()[byte_offset..];
        if bit_widths.len() > block_rel_id {
            // that block is bitpacked.
            let bit_width = bit_widths[block_rel_id];
            self.block_decoder.uncompress_block_unsorted(compressed_data, bit_width);
        } else {
            // that block is vint encoded.
            unimplemented!();
            // self.block_decoder.uncompress_vint_unsorted(compressed_data);
        }
        self.block_offset = self.anchor_offset + (block_rel_id * COMPRESSION_BLOCK_SIZE) as u64;
   }

    /// Fills a buffer with the positions `[offset..offset+output.len())` integers.
    ///
    /// `offset` is required to have a value >= to the offsets given in previous calls
    /// for the given `PositionReaderAbsolute` instance.
    pub fn read(&mut self, mut offset: u64, mut output: &mut [u32]) {
        assert!(
            offset >= self.anchor_offset,
            "offset arguments should be increasing."
        );
        let delta_to_block_offset = offset as i64 - self.block_offset as i64;
        if !(0..128).contains(&delta_to_block_offset) {
            // The first position is not within the first block.
            // (Note that it could be before or after)
            // We need to possibly skip a few blocks, and decompress the first relevant  block.
            let delta_to_anchor_offset = offset - self.anchor_offset;
            let num_blocks_to_skip =
                (delta_to_anchor_offset / (COMPRESSION_BLOCK_SIZE as u64)) as usize;
            self.advance_num_blocks(num_blocks_to_skip);
            self.load_block(0);
        } else {
            // The request offset is within the loaded block.
            // We still need to advance anchor_offset to our current block.
            let num_blocks_to_skip =
                ((self.block_offset - self.anchor_offset) / COMPRESSION_BLOCK_SIZE as u64) as usize;
            self.advance_num_blocks(num_blocks_to_skip);
        }

        // At this point, the block containing offset is loaded, and anchor has
        // been updated to point to it as well.
        for i in 1.. {
            // we copy the part from block i - 1 that is relevant.
            let offset_in_block = (offset as usize) % COMPRESSION_BLOCK_SIZE;
            let remaining_in_block = COMPRESSION_BLOCK_SIZE - offset_in_block;
            if remaining_in_block >= output.len() {
                output.copy_from_slice(&self.block_decoder.output_array()[offset_in_block..][..output.len()]);
                break;
            }
            output[..remaining_in_block].copy_from_slice(&self.block_decoder.output_array()[offset_in_block..]);
            output = &mut output[remaining_in_block..];
            // we load block #i if necessary.
            offset += remaining_in_block as u64;
            self.load_block(i);
        }
    }
}
