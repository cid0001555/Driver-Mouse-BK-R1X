#[cfg(windows)]
#[path = "../win_exe_resources.rs"]
mod win_exe_resources;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    win_exe_resources::embed_exe_resources(
        "bkr1x-app.ico",
        "Fire Phoenix BK-R1X Driver",
        "Fire Phoenix BK-R1X System Tray & Hardware Agent",
        [247, 162, 122, 255],
        false,
    )?;
    Ok(())
}
