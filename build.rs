use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(&["src/update_metadata.proto"], &["src/"])?;
    Ok(())
}
