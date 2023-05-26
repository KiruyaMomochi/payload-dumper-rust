mod extent;

use std::io::{SeekFrom, Read, Seek, Write, BufReader};
use binrw::{binrw, BinRead, BinResult, parser};
use chromeos_update_engine::DeltaArchiveManifest;
use extent::SectionFile;
use prost::Message;

use crate::extent::{FragmentFile};

// Include the `chromeos_update_engine` module, which is generated from update_metadata.proto.
pub mod chromeos_update_engine {
    include!(concat!(env!("OUT_DIR"), "/chromeos_update_engine.rs"));
}

/// Update file format: An update file contains all the operations needed
/// to update a system to a specific version. It can be a full payload which
/// can update from any version, or a delta payload which can only update
/// from a specific version.
/// The update format is represented by this struct pseudocode:
/// ```text
/// struct delta_update_file {
///   char magic[4] = "CrAU";
///   uint64 file_format_version;  // payload major version
///   uint64 manifest_size;  // Size of protobuf DeltaArchiveManifest
///
///   // Only present if format_version >= 2:
///   uint32 metadata_signature_size;
///
///   // The DeltaArchiveManifest protobuf serialized, not compressed.
///   char manifest[manifest_size];
///
///   // The signature of the metadata (from the beginning of the payload up to
///   // this location, not including the signature itself). This is a serialized
///   // Signatures message.
///   char metadata_signature_message[metadata_signature_size];
///
///   // Data blobs for files, no specific format. The specific offset
///   // and length of each data blob is recorded in the DeltaArchiveManifest.
///   struct {
///     char data[];
///   } blobs[];
///
///   // The signature of the entire payload, everything up to this location,
///   // except that metadata_signature_message is skipped to simplify signing
///   // process. These two are not signed:
///   uint64 payload_signatures_message_size;
///   // This is a serialized Signatures message.
///   char payload_signatures_message[payload_signatures_message_size];
///
/// };
/// ```
#[derive(BinRead, Debug)]
#[br(big, magic = b"CrAU")]
#[allow(dead_code)]
pub struct DeltaUpdateFile {
    /// Payload major version.
    pub file_format_version: u64,
    /// Size of protobuf DeltaArchiveManifest.
    pub manifest_size: u64,
    /// Size of metadata signature.
    /// Only present if file_format_version >= 2.
    #[br(if(file_format_version >= 2))]
    pub metadata_signature_size: u32,
    /// DeltaArchiveManifest protobuf serialized, not compressed.
    #[br(count = manifest_size, 
         try_map = |x: Vec<u8>| DeltaArchiveManifest::decode(&x[..]))]
    pub manifest: DeltaArchiveManifest,
    /// The signature of the metadata (from the beginning of the payload up to
    /// this location, not including the signature itself). This is a serialized
    /// Signatures message.
    #[br(count = metadata_signature_size)]
    pub metadata_signature_message: Vec<u8>,
    /// Data blobs for files, no specific format. The specific offset
    /// and length of each data blob is recorded in the DeltaArchiveManifest.
    /// We save offset to this data blob in payload file
    #[br(parse_with = current_pos)]
    pub blobs_offset: u64,
    /// The signature of the entire payload, everything up to this location,
    /// except that metadata_signature_message is skipped to simplify signing
    /// process.
    /// 
    /// We don't use `payload_signatures_message_size` because we need calculate
    /// the size of blobs in advance. And I can't find this size in my payload.
    #[br(if(manifest.signatures_offset.is_some() && manifest.signatures_size.is_some()), 
         seek_before = SeekFrom::Current(manifest.signatures_offset.unwrap() as i64),
         count = manifest.signatures_size.unwrap())]
    pub payload_signatures_message_data: Vec<u8>,
}

#[parser(reader)]
fn current_pos() -> BinResult<u64> {
    Ok(reader.stream_position()?)
}

pub fn dump_operation<R: Read + Seek, W: Write + Seek>(
    src: &mut R, 
    src_blobs_offset: u64, 
    dst: &mut W, 
    operation: &chromeos_update_engine::InstallOperation,
    block_size: u64) -> Result<(), Box<dyn std::error::Error>> {

    let data = operation.data_offset
        .zip(operation.data_length)
        .ok_or_else(|| "no data".to_string())
        .and_then(|(offset, length)| {
            SectionFile::new(src, src_blobs_offset + offset, length)
                .map_err(|e| e.to_string())
        });

    // println!("\n{} - {}\n", operation.data_offset(), operation.data_length());
    // let mut file = std::fs::File::create("dump.bin")?;
    // std::io::copy(&mut data?, &mut file);

    let dst = if operation.dst_extents.is_empty() {
        Err("no dst extents")
    } else {
        Ok(FragmentFile::new_from_extents(dst, &operation.dst_extents, block_size)?)
    };

    match operation.r#type() {
        // REPLACE: Replace the dst_extents on the drive with the attached data,
        // zero padding out to block size.
        chromeos_update_engine::install_operation::Type::Replace => {
            let mut dst = dst?;

            let copied = std::io::copy(&mut data?, &mut dst)?;
            assert_eq!(copied, operation.data_length());
            assert_eq!(copied, dst.size());
        },
        // REPLACE_BZ: bzip2-uncompress the attached data and write it into
        // dst_extents on the drive, zero padding to block size.
        chromeos_update_engine::install_operation::Type::ReplaceBz => {
            let mut dst = dst?;

            let mut data = BufReader::new(data?);
            libribzip2::stream::decode_stream(&mut data, &mut dst).map_err(|()| "bzip2 error")?;
            let copied = dst.seek(SeekFrom::Current(0))?;
            // let mut decoder = bzip2_rs::DecoderReader::new(data?);
            // let copied = std::io::copy(&mut decoder, &mut dst)?;
            assert_eq!(copied, dst.size());
        },
        // REPLACE_XZ: Replace the dst_extents with the contents of the attached
        // xz file after decompression. The xz file should only use crc32 or no crc at
        // all to be compatible with xz-embedded.
        chromeos_update_engine::install_operation::Type::ReplaceXz => {
            let mut data = BufReader::new(data?);
            let mut dst = dst?;

            lzma_rs::xz_decompress(&mut data, &mut dst)?;
            let size_write = dst.seek(SeekFrom::Current(0))?;
            assert_eq!(size_write, dst.size());
        },
        // ZERO: Write zeros to the destination dst_extents.
        chromeos_update_engine::install_operation::Type::Zero => {
            let mut dst = dst?;
            let mut zeros = std::io::repeat(0u8).take(dst.size());
            std::io::copy(&mut zeros, &mut dst)?;
        },
        // DISCARD: Discard the destination dst_extents blocks on the physical medium.
        // the data read from those blocks is undefined.
        chromeos_update_engine::install_operation::Type::Discard => {},
        // MOVE: Copy the data in src_extents to dst_extents. Extents may overlap,
        // so it may be desirable to read all src_extents data into memory before
        // writing it out. (deprecated)
        chromeos_update_engine::install_operation::Type::Move => todo!("src_extents"),
        // SOURCE_COPY: Copy the data in src_extents in the old partition to
        // dst_extents in the new partition. There's no overlapping of data because
        // the extents are in different partitions.
        chromeos_update_engine::install_operation::Type::SourceCopy => todo!("src_extents"),
        // BSDIFF: Read src_length bytes from src_extents into memory, perform
        // bspatch with attached data, write new data to dst_extents, zero padding
        // to block size. (deprecated)
        chromeos_update_engine::install_operation::Type::Bsdiff => todo!("diff"),
        // SOURCE_BSDIFF: Read the data in src_extents in the old partition, perform
        // bspatch with the attached data and write the new data to dst_extents in the
        // new partition.
        chromeos_update_engine::install_operation::Type::SourceBsdiff => todo!("diff"),
        // Like SOURCE_BSDIFF, but compressed with brotli.
        chromeos_update_engine::install_operation::Type::BrotliBsdiff => todo!("diff"),
        // PUFFDIFF: Read the data in src_extents in the old partition, perform
        // puffpatch with the attached data and write the new data to dst_extents in
        // the new partition.
        chromeos_update_engine::install_operation::Type::Puffdiff => todo!("diff"),
    }

    Ok(())
}