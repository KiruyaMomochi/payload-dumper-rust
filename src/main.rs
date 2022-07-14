use std::{
    fs::File,
    path::PathBuf,
};

use binrw::BinReaderExt;
use indicatif::{ProgressBar, ProgressStyle};
use payload_dumper_rust::{dump_operation, DeltaUpdateFile};

use clap::Parser;
use size::Size;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to the update file
    #[clap(default_value = "payload.bin", value_parser)]
    path: PathBuf,

    /// Directory to output the dump
    #[clap(default_value = "output", short, long, value_parser)]
    output: PathBuf,

    /// Partitions to dump
    #[clap(short, long)]
    partitions: Option<Vec<String>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut file = File::open(args.path)?;
    let payload: DeltaUpdateFile = file.read_be()?;

    let partitions = payload
        .manifest
        .partitions
        .iter()
        .map(partiotion_to_string)
        .collect::<Vec<_>>()
        .join(" ");
    println!("Partitions: {}", partitions);

    let partitions: Vec<_> = if let Some(partitions) = args.partitions {
        let mut result = Vec::new();
        for partition in partitions {
            match payload
                .manifest
                .partitions
                .iter()
                .find(|p| p.partition_name == partition)
            {
                Some(partition) => result.push(partition),
                None => return Err(format!("Partition {} not found", partition).into()),
            }
        }
        result
    } else {
        payload.manifest.partitions.iter().collect()
    };

    if !args.output.is_dir() {
        std::fs::create_dir_all(&args.output)?;
    }

    let style = ProgressStyle::default_bar()
        .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}");

    // The client will perform each InstallOperation in order, beginning even
    // before the entire delta file is downloaded (but after at least the
    // protobuf is downloaded).
    for partition in partitions {
        let bar = ProgressBar::new(partition.operations.len() as u64);
        bar.set_style(style.clone());

        let img = args
            .output
            .join(format!("{}.img", partition.partition_name));
        let mut img = File::create(img)?;

        for operation in &partition.operations {
            bar.set_message(format!(
                "{}: {:?}",
                partition.partition_name,
                operation.r#type()
            ));
            bar.inc(1);
            dump_operation(
                &mut file,
                payload.blobs_offset,
                &mut img,
                operation,
                payload.manifest.block_size.unwrap() as u64,
            )?;
        }

        bar.finish();
    }

    Ok(())
}

fn partiotion_to_string(
    x: &payload_dumper_rust::chromeos_update_engine::PartitionUpdate,
) -> String {
    let name = &x.partition_name;
    let part = x
        .new_partition_info
        .as_ref()
        .and_then(|i| i.size)
        .map(Size::from_bytes)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "? MiB".to_string());

    format!("{} ({})", name, part)
}
