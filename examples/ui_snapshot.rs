//! Render the real UI on a development host without starting screen capture.
fn main() -> Result<(), String> {
    let folder = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "docs/ui".into());
    framelink::snapshot::write_gallery(&folder)?;
    println!("UI snapshots: {}", folder.display());
    Ok(())
}
