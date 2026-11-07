#[cfg(windows)]
#[path = "../win_exe_resources.rs"]
mod win_exe_resources;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    win_exe_resources::embed_exe_resources(
        "bkr1x-setup.ico",
        "Fire Phoenix BK-R1X Setup",
        "Fire Phoenix BK-R1X 1-Click Setup Wizard",
        [222, 195, 42, 255],
        true,
    )?;
    Ok(())
}
